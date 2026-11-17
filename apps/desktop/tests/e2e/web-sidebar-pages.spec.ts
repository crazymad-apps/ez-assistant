import { type RuntimeCommand, type RuntimeCommandResult, compatibilityHeaders } from "@ez-assistant/protocol";
import { expect, test } from "@playwright/test";

test.use({ viewport: { width: 1440, height: 900 } });

// 所有写入均通过正式命令进入 globalSetup 新建的隔离 Runtime Home。
test("侧栏项目和会话独立分页，页外置顶、搜索和项目选择不依赖已加载页", async ({ page }, info) => {
  test.setTimeout(120_000);
  const host = JSON.parse(process.env.EZ_ASSISTANT_E2E_BOOTSTRAP!) as { base_url: string; access_token: string };
  const headers = { ...compatibilityHeaders(), Authorization: `Bearer ${host.access_token}`, "Content-Type": "application/json" };
  async function command<T extends RuntimeCommand["type"]>(type: T, payload: object = {}): Promise<Extract<RuntimeCommandResult, { type: T }>["payload"]> {
    const response = await fetch(`${host.base_url}/commands`, {
      method: "POST", headers,
      body: JSON.stringify({ request_id: crypto.randomUUID(), command: { scope: "runtime", payload: { type, payload } } }),
    });
    const body = await response.json();
    expect(response.status, JSON.stringify(body)).toBe(200);
    return body.result.payload.payload;
  }
  const old = (await command("register_workspace", { label: "F04 页外项目", primary_directory: null, additional_directories: [] })).workspace;
  const pinned = (await command("create_session", { title: "F04 页外置顶", workspace_id: old.workspace_id })).session;
  await command("set_session_pinned", { session_id: pinned.session_id, is_pinned: true });
  const off_page = (await command("create_session", { title: "F04 页外搜索会话", workspace_id: old.workspace_id })).session;
  let latest = old;
  for (let i = 0; i < 20; i++) latest = (await command("register_workspace", { label: `F04 项目 ${i.toString().padStart(2, "0")}`, primary_directory: null, additional_directories: [] })).workspace;
  for (let i = 0; i < 22; i++) {
    await command("create_session", { title: `F04 项目会话 ${i.toString().padStart(2, "0")}`, workspace_id: latest.workspace_id });
    await command("create_session", { title: `F04 独立会话 ${i.toString().padStart(2, "0")}` });
  }
  const snapshot = (await command("get_application_snapshot")).snapshot.value;
  expect(snapshot.workspaces).toHaveLength(10);
  expect(snapshot.workspaces_next_offset).toBe(10);
  expect(snapshot.pinned_sessions.map(row => row.session_id)).toContain(pinned.session_id);
  expect(snapshot.workspaces.some(row => row.workspace_id === old.workspace_id)).toBe(false);
  for (const group of snapshot.session_groups.filter(group => group.group.type === "unbound" || group.group.workspace_id === latest.workspace_id)) {
    expect(group.sessions).toHaveLength(10);
    expect(group.next_offset).toBe(10);
    expect(group.sessions[0].title).toMatch(/21$/);
  }
  const login = await fetch(`${host.base_url}/auth/login`, { method: "POST", headers, body: JSON.stringify({ method: "desktop" }) });
  expect(login.ok).toBe(true);
  const { token } = await login.json() as { token: string };
  const errors: string[] = [];
  page.on("pageerror", error => errors.push(error.message));
  await page.emulateMedia({ reducedMotion: "reduce" });
  await page.goto(`${host.base_url}/#token=${encodeURIComponent(token)}`);
  const sidebar = page.getByRole("complementary", { name: "会话导航" });
  await expect(sidebar.getByRole("button", { name: "F04 页外置顶", exact: true })).toBeVisible();
  await expect(sidebar.getByRole("button", { name: "F04 页外项目", exact: true })).toHaveCount(0);
  const project_rows = sidebar.getByRole("button", { name: /^F04 项目 \d{2}$/ });
  const more_projects = sidebar.locator('[data-sidebar-more=\'{"scope":"workspaces"}\']')
    .getByRole("button", { name: "加载更多", exact: true });
  await expect(project_rows).toHaveCount(10);
  await more_projects.click();
  await expect(project_rows).toHaveCount(20);
  const project = sidebar.getByRole("button", { name: latest.label, exact: true }).locator("xpath=ancestor::section[1]");
  await expect(project.getByRole("button", { name: /^F04 项目会话/ })).toHaveCount(10);
  await project.getByRole("button", { name: "加载更多", exact: true }).click();
  await expect(project.getByRole("button", { name: /^F04 项目会话/ })).toHaveCount(20);
  await project.getByRole("button", { name: "加载更多", exact: true }).click();
  await expect(project.getByRole("button", { name: /^F04 项目会话/ })).toHaveCount(22);
  const unbound = sidebar.getByRole("button", { name: "独立会话", exact: true }).locator("xpath=ancestor::section[1]");
  await expect(unbound.getByRole("button", { name: /^F04 独立会话/ })).toHaveCount(10);
  await unbound.getByRole("button", { name: "加载更多", exact: true }).click();
  await expect(unbound.getByRole("button", { name: /^F04 独立会话/ })).toHaveCount(20);
  await unbound.getByRole("button", { name: "加载更多", exact: true }).click();
  await expect(unbound.getByRole("button", { name: /^F04 独立会话/ })).toHaveCount(22);

  await page.getByRole("button", { name: "搜索会话", exact: true }).click();
  const search = page.getByRole("dialog", { name: "搜索会话" });
  await search.getByRole("searchbox", { name: "搜索会话名称" }).fill(off_page.title);
  await search.getByRole("button", { name: new RegExp(off_page.title) }).click();
  await expect(page.getByRole("button", { name: off_page.title, exact: true })).toBeVisible();
  await expect(sidebar.getByRole("button", { name: off_page.title, exact: true })).toHaveCount(0);
  await expect(sidebar.getByRole("button", { name: old.label, exact: true })).toHaveCount(0);

  await sidebar.getByRole("button", { name: "新建会话", exact: true }).click();
  await page.getByRole("button", { name: "新会话项目", exact: true }).click();
  const picker = page.getByRole("dialog", { name: "选择新会话项目" });
  await picker.getByRole("combobox", { name: "搜索项目" }).fill(old.label);
  await picker.getByRole("option", { name: new RegExp(old.label) }).click();
  await expect(page.getByRole("button", { name: "新会话项目", exact: true })).toContainText(old.label);
  await expect(sidebar.getByRole("button", { name: old.label, exact: true })).toHaveCount(0);
  await command("remove_workspace", { workspace_id: old.workspace_id });
  await expect(sidebar.getByRole("button", { name: pinned.title, exact: true })).toHaveCount(0);
  await sidebar.getByRole("button", { name: "恢复已移除项目", exact: true }).click();
  const restore = page.getByRole("dialog", { name: "恢复已移除项目" });
  await restore.getByRole("button", { name: "恢复", exact: true }).click();
  await expect(restore).toBeHidden();
  await expect(page.getByRole("button", { name: "新会话项目", exact: true })).toContainText(old.label);
  await expect(sidebar.getByRole("button", { name: old.label, exact: true })).toHaveCount(0);
  // 恢复事件可以使尚在飞行的分页失效；等待新基线后显式重试读取。
  await expect.poll(async () => {
    if (await sidebar.getByRole("button", { name: old.label, exact: true }).count()) return true;
    if (await more_projects.count() && await more_projects.isEnabled()) await more_projects.click();
    return false;
  }).toBe(true);
  await expect(sidebar.getByRole("button", { name: old.label, exact: true })).toBeVisible();
  const restored_project = sidebar.getByRole("button", { name: old.label, exact: true });
  if (await restored_project.getAttribute("aria-expanded") !== "true") await restored_project.click();
  await expect(sidebar.getByRole("button", { name: off_page.title, exact: true })).toBeVisible();
  await sidebar.getByRole("button", { name: `取消置顶 ${pinned.title}`, exact: true }).click();
  const restored_group = restored_project.locator("xpath=ancestor::section[1]");
  await expect(restored_group.getByRole("button", { name: /^F04 页外(?:置顶|搜索会话)$/ })).toHaveText([off_page.title, pinned.title]);
  await restored_project.scrollIntoViewIfNeeded();
  const scrollTop = () => restored_project.evaluate(element => {
    let parent = element.parentElement;
    while (parent && getComputedStyle(parent).overflowY !== "auto") parent = parent.parentElement;
    return parent?.scrollTop ?? -1;
  });
  const before_scroll = await scrollTop();
  expect(before_scroll).toBeGreaterThan(0);
  await command("rename_session", { session_id: off_page.session_id, title: "F04 更名后位置不变" });
  await expect(restored_group.getByRole("button", { name: "F04 更名后位置不变", exact: true })).toBeVisible();
  expect(Math.abs((await scrollTop()) - before_scroll)).toBeLessThan(2);
  await expect(project.getByRole("button", { name: /^F04 项目会话/ })).toHaveCount(22);
  await expect(unbound.getByRole("button", { name: /^F04 独立会话/ })).toHaveCount(22);
  expect(errors).toEqual([]);
  await page.screenshot({ path: info.outputPath("sidebar-pages.png"), animations: "disabled" });
});
