import { mkdir, mkdtemp, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { compatibilityHeaders, type RuntimeCommand, type RuntimeCommandResult } from "@ez-assistant/protocol";
import { expect, test } from "@playwright/test";

test("五类上下文独立分页，旧消息、附件和子任务可从页外定位", async ({ page }) => {
  test.setTimeout(120_000);
  const host = JSON.parse(process.env.EZ_ASSISTANT_E2E_BOOTSTRAP!) as { base_url: string; access_token: string };
  const directory = await mkdtemp(join(process.env.EZ_ASSISTANT_E2E_NEW_WORKSPACE!, "f01-context-"));
  const headers = { ...compatibilityHeaders(), Authorization: `Bearer ${host.access_token}`, "Content-Type": "application/json" };
  async function command<T extends RuntimeCommand["type"]>(type: T, payload: object): Promise<Extract<RuntimeCommandResult, { type: T }>["payload"]> {
    const response = await fetch(`${host.base_url}/commands`, { method: "POST", headers,
      body: JSON.stringify({ request_id: crypto.randomUUID(), command: { scope: "runtime", payload: { type, payload } } }) });
    const result = await response.json();
    expect(response.ok, JSON.stringify(result)).toBe(true);
    return result.result.payload.payload;
  }
  const old_client = await fetch(`${host.base_url}/commands`, { method: "POST",
    headers: { ...headers, "x-ez-client-version": "0.33.1", "x-ez-min-compatible-version": "0.33.1" },
    body: JSON.stringify({ request_id: "old", command: { scope: "runtime", payload: { type: "list_workspaces", payload: {} } } }) });
  expect(old_client.status).toBe(409);
  expect((await old_client.json()).error.code).toBe("client_too_old");
  const { workspace } = await command("register_workspace", { label: "F01 分页测试项目", primary_directory: directory, additional_directories: [] });
  const { session: { session_id } } = await command("create_session", { title: "F01 长列表", workspace_id: workspace.workspace_id });
  await command("create_session", { title: "F01 分页空会话", workspace_id: workspace.workspace_id });
  await command("set_session_approval_mode", { session_id, approval_mode: "auto" });
  const ids: string[] = [];
  for (let index = 0; index < 12; index += 1) {
    const skill = join(directory, ".agents/skills", `context-skill-${index}`);
    await mkdir(skill, { recursive: true });
    await writeFile(join(skill, "SKILL.md"), `---\nname: context-skill-${index}\ndescription: Fixture skill ${index}\n---\nRead the requested fixture.`);
    await writeFile(join(directory, `deliver-${index}.txt`), `deliverable ${index}`);
    const form = new FormData();
    form.append("file", new Blob([`attachment ${index}`], { type: "text/plain" }), `page-file-${index}.txt`);
    const response = await fetch(`${host.base_url}/sessions/${session_id}/attachments`, { method: "POST",
      headers: { ...compatibilityHeaders(), Authorization: headers.Authorization }, body: form });
    expect(response.ok).toBe(true);
    ids.push((await response.json()).attachment.attachment_id);
  }
  for (let index = 0; index < 12; index += 1) {
    const { input_id } = await command("submit_input", { session_id, message: `F01_CONTEXT_PAGED ${index}`, variant: "build", mode: "normal",
      attachment_ids: index === 0 ? [ids[11]] : [], ...(index === 0 ? { skill_name: "context-skill-11" } : {}), quotes: [] });
    await expect.poll(async () => (await command("list_runs", { session_id, input_id, page: { limit: 1 }, summary_only: true })).runs[0]?.status,
      { timeout: 12_000 }).toBe("completed");
  }
  const snapshot = (await command("get_session_view", { session_id })).snapshot.value;
  expect(snapshot.runs).toHaveLength(1);
  expect(snapshot).not.toHaveProperty("attachments");
  expect(snapshot).not.toHaveProperty("artifacts");
  expect(snapshot.conversation.items.some((item) => item.type === "user" && item.text === "F01_CONTEXT_PAGED 0")).toBe(false);
  expect((await command("list_artifacts", { session_id })).page.total).toBe(12);
  expect((await command("list_child_tasks", { session_id })).page.total).toBe(12);
  const skills = await command("list_skills", { session_id, include_plugins: true, page: { limit: 1 } });
  expect(skills.snapshot.skills[0]?.name).toBe("context-skill-11");
  expect((await command("list_skills", { workspace_id: workspace.workspace_id })).snapshot.skills.length).toBeGreaterThan(10);

  const login = await fetch(`${host.base_url}/auth/login`, { method: "POST", headers, body: JSON.stringify({ method: "desktop" }) });
  expect(login.ok).toBe(true);
  const { token } = await login.json();
  await page.emulateMedia({ reducedMotion: "reduce" });
  await page.goto(`${host.base_url}/#token=${encodeURIComponent(token)}`);
  await page.getByRole("complementary", { name: "会话导航" }).getByRole("button", { name: "F01 长列表", exact: true }).click();
  const expand = page.getByRole("button", { name: "展开资源栏", exact: true });
  if (await expand.isVisible()) await expand.click();
  const pageOf = (label: string) => page.getByRole("navigation", { name: `${label}分页`, exact: true });
  for (const label of ["运行记录", "技能", "子任务", "会话附件", "产物"]) {
    await expect(pageOf(label)).toContainText("1 / 2");
  }
  for (const label of ["技能", "子任务", "产物"]) {
    await pageOf(label).getByRole("button", { name: `下一页${label}` }).click();
    await expect(pageOf(label)).toContainText("2 / 2");
  }
  await expect(pageOf("会话附件")).toContainText("1 / 2");
  await expect(pageOf("运行记录")).toContainText("1 / 2");
  let fail_next = true;
  let hold_attachment_page = false;
  let release_attachment_page: (() => void) | undefined;
  await page.route("**/commands", async (route) => {
    const command = route.request().postDataJSON()?.command?.payload;
    if (command?.type === "list_runs" && command.payload.page?.offset === 10 && fail_next) {
      fail_next = false;
      return route.fulfill({ status: 503, json: { error: { code: "snapshot_busy", message: "分页读取夹具故障" } } });
    }
    if (hold_attachment_page && command?.type === "list_attachments" && command.payload.session_id === session_id) {
      hold_attachment_page = false;
      const response = await route.fetch();
      await new Promise<void>((done) => { release_attachment_page = done; });
      return route.fulfill({ response }).catch(() => undefined);
    }
    return route.continue();
  });
  await pageOf("运行记录").getByRole("button", { name: "下一页运行记录" }).click();
  await expect(page.getByRole("alert").filter({ hasText: "分页读取夹具故障" })).toBeVisible();
  await expect(pageOf("运行记录")).toContainText("1 / 2");
  await page.getByRole("button", { name: "重试读取运行记录" }).click();
  await expect(pageOf("运行记录")).toContainText("2 / 2");
  const run_section = page.getByRole("button", { name: "运行记录 · 12", exact: true }).locator("../..");
  await run_section.getByRole("button", { name: /运行 #1/ }).last().click();
  await expect(page.getByText("F01_CONTEXT_PAGED 0", { exact: true })).toBeVisible();
  await expect(page.getByText("page-file-11.txt", { exact: true })).toBeVisible();
  await page.getByRole("button", { name: "处理过程", exact: true }).first().click();
  await page.getByRole("button", { name: "工具调用：委派子任务", exact: true }).first().click();
  await expect(page.getByRole("button", { name: "查看子任务：分页子任务 0", exact: true })).toBeVisible();
  await expect(pageOf("子任务")).toContainText("2 / 2");
  await pageOf("会话附件").getByRole("button", { name: "下一页会话附件" }).click();
  await expect(pageOf("会话附件")).toContainText("2 / 2");

  hold_attachment_page = true;
  await page.evaluate(() => window.dispatchEvent(new Event("focus")));
  await expect.poll(() => Boolean(release_attachment_page)).toBe(true);
  await page.getByRole("complementary", { name: "会话导航" }).getByRole("button", { name: "F01 分页空会话", exact: true }).click();
  release_attachment_page?.();
  await expect(page.getByRole("button", { name: "会话附件 · 0", exact: true })).toBeVisible();
  await expect(page.getByRole("button", { name: /page-file-11\.txt/ })).toHaveCount(0);

  const context = page.getByRole("tabpanel", { name: "当前上下文" });
  const empty_style = (element: Element) => {
    const { borderStyle, borderWidth, borderColor, borderRadius, padding, textAlign, fontSize, color } = getComputedStyle(element);
    return { borderStyle, borderWidth, borderColor, borderRadius, padding, textAlign, fontSize, color };
  };
  const attachment_empty = context.getByText("暂无会话附件", { exact: true });
  const reference_style = await attachment_empty.evaluate(empty_style);
  expect(reference_style).toMatchObject({ borderStyle: "dashed", padding: "8px", textAlign: "center" });
  for (const message of ["没有后台运行。", "尚无产物", "暂无子任务", "还没有运行记录"]) {
    const empty = context.getByText(message, { exact: true });
    await expect(empty).toBeVisible();
    expect(await empty.evaluate(empty_style)).toEqual(reference_style);
  }
});
