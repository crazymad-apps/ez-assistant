import { mkdtemp, unlink, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { compatibilityHeaders, type RuntimeCommand, type RuntimeCommandResult } from "@ez-assistant/protocol";
import { expect, test } from "@playwright/test";

test("后台列表分页，输出上翻锚定、暂停、恢复与结束后回收", async ({ page }) => {
  test.skip(process.platform === "win32", "离线 Shell 夹具使用 POSIX 命令");
  test.setTimeout(55_000);
  const host = JSON.parse(process.env.EZ_ASSISTANT_E2E_BOOTSTRAP!) as { base_url: string; access_token: string };
  const directory = await mkdtemp(join(process.env.EZ_ASSISTANT_E2E_NEW_WORKSPACE!, "f01-runs-"));
  const headers = { ...compatibilityHeaders(), Authorization: `Bearer ${host.access_token}`, "Content-Type": "application/json" };
  async function command<T extends RuntimeCommand["type"]>(type: T, payload: object): Promise<Extract<RuntimeCommandResult, { type: T }>["payload"]> {
    const response = await fetch(`${host.base_url}/commands`, {
      method: "POST", headers, body: JSON.stringify({ request_id: crypto.randomUUID(), command: { scope: "runtime", payload: { type, payload } } }),
    });
    const body = await response.json();
    expect(response.ok, JSON.stringify(body)).toBe(true);
    return body.result.payload.payload;
  }

  const { workspace } = await command("register_workspace", { label: "F01 后台输出夹具", primary_directory: directory, additional_directories: [] });
  const { session } = await command("create_session", { title: "F01 后台列表", workspace_id: workspace.workspace_id });
  const { session: other } = await command("create_session", { title: "F01 空会话", workspace_id: workspace.workspace_id });
  const session_id = session.session_id;
  await command("set_session_approval_mode", { session_id, approval_mode: "auto" });
  const login = await fetch(`${host.base_url}/auth/login`, { method: "POST", headers, body: JSON.stringify({ method: "desktop" }) });
  expect(login.ok).toBe(true);
  const { token } = await login.json();
  await page.setViewportSize({ width: 1600, height: 900 });
  await page.emulateMedia({ reducedMotion: "reduce" });
  await page.goto(`${host.base_url}/#token=${encodeURIComponent(token)}`);
  const sidebar = page.getByRole("complementary", { name: "会话导航" });
  const select = (title: string) => sidebar.getByRole("button", { name: title, exact: true }).click();
  const heading = (total?: number) => page.getByRole("button", { name: total === undefined ? "后台运行" : `后台运行 · ${total}`, exact: true });

  const reads: Readonly<{ cursor?: number; offset_chars?: number }>[] = [];
  let hold_initial_list = true;
  let release_initial_list: (() => void) | undefined;
  let fail_next_follow = false;
  let fail_next_stop = false;
  let mark_next_older_dropped = true;
  let hold_next_follow = false;
  let release_follow: (() => void) | undefined;
  await page.route("**/commands", async (route) => {
    const body = route.request().postDataJSON()?.command?.payload;
    if (hold_initial_list && body?.type === "list_background_runs" && body.payload.session_id === session_id) {
      hold_initial_list = false;
      const response = await route.fetch();
      await new Promise<void>((done) => { release_initial_list = done; });
      return route.fulfill({ response }).catch(() => undefined);
    }
    if (body?.type === "stop_background_run" && fail_next_stop) {
      fail_next_stop = false;
      return route.fulfill({ status: 503, json: { error: { code: "background_cleanup_unconfirmed", message: "停止测试故障" } } });
    }
    if (body?.type !== "read_background_output") return route.continue();
    reads.push(body.payload);
    if (body.payload.cursor == null) return route.continue();
    if (body.payload.offset_chars < 0 && mark_next_older_dropped) {
      mark_next_older_dropped = false;
      const response = await route.fetch();
      const json = await response.json();
      const output = json.result.payload.payload;
      output.cursor += 17;
      output.dropped_chars = 17;
      output.content = [...output.content].slice(17).join("");
      return route.fulfill({ response, json });
    }
    if (!(body.payload.offset_chars < 0) && fail_next_follow) {
      fail_next_follow = false;
      return route.fulfill({ status: 503, json: { error: { code: "background_output_unavailable", message: "输出读取测试故障" } } });
    }
    if (!(body.payload.offset_chars < 0) && hold_next_follow) {
      hold_next_follow = false;
      const response = await route.fetch();
      await new Promise<void>((done) => { release_follow = done; });
      return route.fulfill({ response }).catch(() => undefined);
    }
    return route.continue();
  });

  await select("F01 后台列表");
  const open_panel = page.getByRole("button", { name: "展开资源栏", exact: true });
  if (await open_panel.isVisible()) await open_panel.click();
  await expect.poll(() => Boolean(release_initial_list)).toBe(true);
  await expect(heading()).toBeVisible();
  await expect(heading(0)).toHaveCount(0);
  release_initial_list?.();
  await expect(heading(0)).toBeVisible();
  await heading(0).click();

  await command("submit_input", { session_id, message: "F01_BACKGROUND_JOBS", mode: "normal", variant: "build", attachment_ids: [], quotes: [] });
  await expect(heading(6)).toHaveAttribute("aria-expanded", "false");
  const context = page.getByRole("tabpanel", { name: "当前上下文" });
  const columns = context.locator('[data-layout="two-columns"]');
  const splitter = page.getByRole("separator", { name: "调整资源栏宽度" });
  await splitter.press("End");
  await expect(columns).toHaveCount(1);
  await expect(heading(6)).toHaveAttribute("aria-expanded", "false");
  await splitter.press("Home");
  await expect(columns).toHaveCount(0);
  await expect(heading(6)).toHaveAttribute("aria-expanded", "false");
  await splitter.dblclick();
  await heading(6).click();
  const rows = page.getByRole("list", { name: "后台运行列表" }).getByRole("listitem");
  await expect(rows).toHaveCount(5);
  await page.getByRole("button", { name: "下一页后台运行" }).click();
  await expect(rows).toHaveCount(1);
  await expect(page.getByRole("navigation", { name: "后台运行分页" })).toContainText("2 / 2");
  const open_run = rows.first().getByRole("button", { name: /BOOT/ });
  const job = (await open_run.getAttribute("title"))?.match(/BOOT(\d)/)?.[1];
  expect(job).toBeDefined();
  const stop = rows.first().getByRole("button", { name: "停止", exact: true });
  const history_row = context.getByRole("button", { name: /^运行 #\d/ }).first();
  await expect(history_row).toBeVisible();
  for (const zoom of [1, 1.25, 1.5]) {
    await page.evaluate((zoom) => { document.documentElement.style.zoom = String(zoom); }, zoom);
    const history = (await history_row.elementHandle())!;
    const styles = await open_run.evaluate((button, history) => {
      function rowStyle(row: Element, trigger: Element, trailing: Element) {
        const [dot, details] = trigger.children;
        const [title, metadata] = details.children;
        const row_box = row.getBoundingClientRect();
        const dot_box = dot.getBoundingClientRect();
        const textStyle = (element: Element) => {
          const { fontSize, fontWeight, color } = getComputedStyle(element);
          return { fontSize, fontWeight, color };
        };
        return {
          height: row_box.height,
          radius: getComputedStyle(row).borderRadius,
          marker_size: dot_box.width,
          marker_left: dot_box.left - row_box.left,
          details_left: details.getBoundingClientRect().left - row_box.left,
          title: textStyle(title),
          metadata: textStyle(metadata),
          trailing: textStyle(trailing),
          title_height: title.getBoundingClientRect().height,
          metadata_height: metadata.getBoundingClientRect().height,
        };
      }
      return {
        background: rowStyle(button.parentElement!, button, button.nextElementSibling!),
        history: rowStyle(history, history, history.lastElementChild!),
      };
    }, history);
    expect(styles.background).toEqual(styles.history);
  }
  await page.evaluate(() => { document.documentElement.style.zoom = ""; });
  const alignment = await open_run.evaluate((button) => {
    const row = button.parentElement!.getBoundingClientRect();
    const [command, metadata] = button.children[1].children;
    const command_box = command.getBoundingClientRect();
    const metadata_box = metadata.getBoundingClientRect();
    const stop_text = document.createRange();
    stop_text.selectNodeContents(button.nextElementSibling!);
    const stop_box = stop_text.getBoundingClientRect();
    const row_center = (row.top + row.bottom) / 2;
    return {
      command_height: command_box.height,
      command_font_size: parseFloat(getComputedStyle(command).fontSize),
      task_offset: Math.abs((command_box.top + metadata_box.bottom) / 2 - row_center),
      stop_offset: Math.abs((stop_box.top + stop_box.bottom) / 2 - row_center),
    };
  });
  expect(alignment.command_height).toBeGreaterThanOrEqual(alignment.command_font_size);
  expect(alignment.task_offset).toBeLessThanOrEqual(1);
  expect(alignment.stop_offset).toBeLessThanOrEqual(2);
  await stop.hover();
  expect(await stop.evaluate((button) => ({ row: getComputedStyle(button.parentElement!).backgroundColor, own: getComputedStyle(button).backgroundColor }))).toMatchObject({ own: "rgba(0, 0, 0, 0)" });
  await expect.poll(() => stop.evaluate((button) => getComputedStyle(button.parentElement!).backgroundColor)).not.toBe("rgba(0, 0, 0, 0)");
  await stop.focus();
  expect(await stop.evaluate((button) => button.parentElement!.matches(":focus-within"))).toBe(true);
  await open_run.click();

  const dialog = page.getByRole("dialog", { name: "后台输出" });
  const log = dialog.getByRole("region", { name: "后台日志" });
  const output = log.locator("pre:has([data-output-cursor])");
  await expect(dialog.getByText(/exit 7/)).toBeVisible();
  await expect(output).toContainText(`TAIL${job}`);
  const distanceFromBottom = () => log.evaluate((element) => element.scrollHeight - element.clientHeight - element.scrollTop);
  await expect.poll(distanceFromBottom).toBeLessThanOrEqual(2);
  const first = log.locator("[data-output-cursor]").first();
  const initial_cursor = await first.getAttribute("data-output-cursor");
  const anchor = log.locator(`[data-output-cursor="${initial_cursor}"]`);
  await log.evaluate((element) => { element.scrollTop = 0; });
  await expect.poll(() => first.getAttribute("data-output-cursor")).not.toBe(initial_cursor);
  const anchor_offset = await anchor.evaluate((element) => element.getBoundingClientRect().top - element.closest('[role="region"]')!.getBoundingClientRect().top);
  expect(Math.abs(anchor_offset - 11)).toBeLessThan(4);
  expect(reads.some((read) => (read.offset_chars ?? 0) < 0)).toBe(true);
  await expect(dialog.getByRole("status").filter({ hasText: "本次跳过 17 个字符" })).toBeVisible();
  const paused_read_count = reads.length;
  await writeFile(join(directory, `f01-go-${job}`), "ready");
  await expect.poll(async () => (await command("read_background_output", {
    session_id, background_id: (await command("list_background_runs", { session_id, offset: 5, limit: 1 })).runs[0].background_id,
  })).content).toContain(`UPDATE${job}`);
  // 给前向轮询一个完整周期，确认上翻期间没有追读。
  await new Promise((done) => setTimeout(done, 900));
  expect(reads).toHaveLength(paused_read_count);
  await expect(output).not.toContainText(`UPDATE${job}`);

  fail_next_follow = true;
  await log.evaluate((element) => { element.scrollTop = element.scrollHeight; });
  await expect(dialog.getByRole("alert")).toContainText("输出读取测试故障");
  await expect(output).toContainText(`TAIL${job}`);
  await dialog.getByRole("button", { name: "重试读取", exact: true }).click();
  await expect(output).toContainText(`UPDATE${job}`);
  await expect.poll(distanceFromBottom).toBeLessThanOrEqual(2);

  const original_size = page.viewportSize()!;
  await page.setViewportSize({ width: 900, height: 520 });
  await expect(dialog).toBeVisible();
  const bounds = await dialog.boundingBox();
  expect(bounds).not.toBeNull();
  expect(bounds!.height + bounds!.y).toBeLessThanOrEqual(520);
  await expect.poll(distanceFromBottom).toBeLessThanOrEqual(2);
  await page.setViewportSize(original_size);

  hold_next_follow = true;
  await expect.poll(() => Boolean(release_follow)).toBe(true);
  await dialog.getByRole("button", { name: "关闭后台输出" }).click();
  await expect(dialog).toHaveCount(0);
  await select("F01 空会话");
  release_follow?.();
  await expect(page.getByRole("button", { name: "后台运行 · 0", exact: true })).toBeVisible();
  expect((await command("list_background_runs", { session_id: other.session_id })).total).toBe(0);
  expect((await command("list_background_runs", { session_id })).total).toBe(6);
  await select("F01 后台列表");
  await page.getByRole("button", { name: "下一页后台运行" }).click();
  await open_run.focus();
  await open_run.press("Enter");
  await expect(output).toContainText(`UPDATE${job}`);
  await writeFile(join(directory, `f01-exit-${job}`), "ready");
  await expect(dialog).toHaveCount(0);
  await expect(page.getByRole("status").filter({ hasText: "后台命令已结束，退出码 7。" })).toBeVisible();
  await expect(page.getByRole("button", { name: "后台运行 · 5", exact: true })).toBeVisible();
  await expect(page.getByRole("button", { name: "后台运行 · 5", exact: true })).toBeFocused();
  await expect(rows).toHaveCount(5);
  await expect(page.getByRole("navigation", { name: "后台运行分页" })).toHaveCount(0);
  fail_next_stop = true;
  await stop.click();
  await expect(rows.first().getByRole("alert")).toContainText("停止测试故障");
  await expect(rows).toHaveCount(5);
  await rows.first().getByRole("button", { name: "重试停止", exact: true }).click();
  await expect(dialog).toHaveCount(0);
  await expect(page.getByRole("button", { name: "后台运行 · 4", exact: true })).toBeVisible();
  const ended_notice = context.getByRole("status").filter({ hasText: "后台命令已结束" });
  await expect(ended_notice).toBeVisible();
  await unlink(join(directory, `f01-exit-${job}`));
  await command("submit_input", { session_id, message: "F01_BACKGROUND_JOBS", mode: "normal", variant: "build", attachment_ids: [], quotes: [] });
  await expect(heading(10)).toBeVisible();
  await expect(ended_notice).toHaveCount(0);
  await page.reload();
  await select("F01 后台列表");
  await expect(heading(10)).toBeVisible();
  await heading(10).click();
  for (const run of (await command("list_background_runs", { session_id })).runs) {
    await command("stop_background_run", { session_id, background_id: run.background_id });
  }
  await expect(heading(0)).toHaveAttribute("aria-expanded", "false");
});
