import { type RuntimeCommand, type RuntimeCommandResult, compatibilityHeaders } from "@ez-assistant/protocol";
import { expect, test, type Locator, type Page } from "@playwright/test";

test.use({ viewport: { width: 1440, height: 1000 } });

async function drag(page: Page, source: Locator, target: Locator, placement: "before" | "after" = "before", cancel?: "Escape" | "blur") {
  await expect(source).toHaveAttribute("aria-busy", "false");
  await source.scrollIntoViewIfNeeded();
  const a = (await source.boundingBox())!;
  const b = (await target.boundingBox())!;
  await page.mouse.move(a.x + 70, a.y + a.height / 2);
  await page.mouse.down();
  await page.waitForTimeout(150);
  await page.mouse.move(a.x + 70, a.y + a.height / 2 + 9);
  await page.mouse.move(b.x + 70, b.y + (placement === "before" ? 3 : b.height - 3), { steps: 12 });
  if (cancel === "Escape") await page.keyboard.press("Escape");
  if (cancel === "blur") await page.evaluate(() => window.dispatchEvent(new Event("blur")));
  await page.mouse.up();
}

// globalSetup 创建全新隔离 Runtime Home；所有业务写入经正式 Host。
test("拖动四个 Host 范围、取消、失败回退与三个恢复默认入口", async ({ page }, info) => {
  test.setTimeout(120_000);
  const host = JSON.parse(process.env.EZ_ASSISTANT_E2E_BOOTSTRAP!) as { base_url: string; access_token: string };
  const headers = { ...compatibilityHeaders(), Authorization: `Bearer ${host.access_token}`, "Content-Type": "application/json" };
  async function command<T extends RuntimeCommand["type"]>(type: T, payload: object = {}): Promise<Extract<RuntimeCommandResult, { type: T }>["payload"]> {
    const response = await fetch(`${host.base_url}/commands`, { method: "POST", headers, body: JSON.stringify({ request_id: crypto.randomUUID(), command: { scope: "runtime", payload: { type, payload } } }) });
    const body = await response.json();
    expect(response.status, JSON.stringify(body)).toBe(200);
    return body.result.payload.payload;
  }
  const projects = [];
  for (const name of ["A", "B", "C"]) projects.push((await command("register_workspace", { label: `排序项目 ${name}`, primary_directory: null, additional_directories: [] })).workspace);
  const group = projects[2];
  const sessions = [];
  const unbound = [];
  for (const name of ["A", "B", "C"]) {
    sessions.push((await command("create_session", { title: `组内排序 ${name}`, workspace_id: group.workspace_id })).session);
    unbound.push((await command("create_session", { title: `独立排序 ${name}` })).session);
  }
  const pins = [];
  for (const name of ["A", "B"]) {
    const session = (await command("create_session", { title: `置顶排序 ${name}` })).session;
    pins.push(session);
    await command("set_session_pinned", { session_id: session.session_id, is_pinned: true });
  }
  const login = await fetch(`${host.base_url}/auth/login`, { method: "POST", headers, body: JSON.stringify({ method: "desktop" }) });
  const { token } = await login.json() as { token: string };
  const errors: string[] = [];
  const writes: string[] = [];
  page.on("pageerror", error => errors.push(error.message));
  page.on("request", request => {
    if (request.url().endsWith("/commands") && request.postData()?.includes('"move_sidebar_item"')) writes.push(request.postData()!);
  });
  await page.emulateMedia({ reducedMotion: "reduce" });
  await page.goto(`${host.base_url}/#token=${encodeURIComponent(token)}`);
  const sidebar = page.getByRole("complementary", { name: "会话导航" });
  const row = (id: string) => sidebar.locator(`[data-sidebar-item="${id}"]`);
  const projectRows = sidebar.locator('[data-sidebar-scope=\'{"scope":"workspaces"}\']');
  const ids = (rows: Locator) => rows.evaluateAll(nodes => nodes.map(n => n.getAttribute("data-sidebar-item")));
  await expect(row(projects[0].workspace_id)).toBeVisible();
  for (const project of projects) {
    const button = row(project.workspace_id).locator("[data-sidebar-handle]");
    if (await button.getAttribute("aria-expanded") === "true") await button.click();
  }
  const resetBox = (await sidebar.getByRole("button", { name: "恢复项目默认顺序" }).boundingBox())!;
  const restoreBox = (await sidebar.getByRole("button", { name: "恢复已移除项目" }).boundingBox())!;
  expect(Math.abs(resetBox.y - restoreBox.y)).toBeLessThan(1);
  expect(resetBox.x).toBeGreaterThan(restoreBox.x);
  const initialProjects = await ids(projectRows);
  const project_toggle = row(projects[0].workspace_id).locator("[data-sidebar-handle]");
  const toggle_box = (await project_toggle.boundingBox())!;
  const click_x = toggle_box.x + 70;
  const click_y = toggle_box.y + toggle_box.height / 2;
  await page.mouse.move(click_x, click_y);
  await page.mouse.down();
  await page.waitForTimeout(150);
  // 按住后出现小幅位移仍是项目点击，应能展开且不发排序命令。
  await page.mouse.move(click_x, click_y + 6);
  await expect(sidebar.locator('[data-sidebar-item][data-sidebar-dragging="true"]')).toHaveCount(0);
  await page.mouse.up();
  await expect(project_toggle).toHaveAttribute("aria-expanded", "true");
  expect(writes).toHaveLength(0);
  await project_toggle.click();
  await drag(page, row(projects[0].workspace_id), row(projects[2].workspace_id), "before", "Escape");
  expect(await ids(projectRows)).toEqual(initialProjects);
  await drag(page, row(projects[0].workspace_id), row(projects[2].workspace_id), "before", "blur");
  expect(writes).toHaveLength(0);
  await drag(page, row(projects[0].workspace_id), row(projects[2].workspace_id));
  await expect.poll(async () => (await command("list_workspaces")).workspaces[0].workspace_id).toBe(projects[0].workspace_id);
  await expect.poll(() => ids(projectRows)).toEqual([projects[0].workspace_id, projects[2].workspace_id, projects[1].workspace_id, ...initialProjects.slice(3)]);
  await sidebar.getByRole("button", { name: "恢复项目默认顺序" }).click();
  await page.getByRole("dialog", { name: "恢复项目的默认顺序？" }).getByRole("button", { name: "恢复默认", exact: true }).click();
  await expect.poll(() => ids(projectRows)).toEqual(initialProjects);

  const projectButton = row(group.workspace_id).locator("[data-sidebar-handle]");
  await projectButton.click();
  const groupRows = row(group.workspace_id).locator("xpath=ancestor::section[1]").locator('[data-sidebar-item]').filter({ has: page.locator('[data-navigation-action]') });
  await expect(row(sessions[0].session_id)).toBeVisible();
  await drag(page, row(sessions[0].session_id), row(sessions[2].session_id));
  await expect.poll(() => ids(groupRows)).toEqual([sessions[0].session_id, sessions[2].session_id, sessions[1].session_id]);
  await row(group.workspace_id).getByRole("button", { name: `${group.label} 项目操作` }).click();
  await page.getByRole("menuitem", { name: "恢复默认顺序", exact: true }).click();
  await page.getByRole("dialog", { name: `恢复“${group.label}”内会话的默认顺序？` }).getByRole("button", { name: "恢复默认", exact: true }).click();
  await expect.poll(() => ids(groupRows)).toEqual([...sessions].reverse().map(s => s.session_id));
  await projectButton.click();

  const independent = sidebar.getByRole("button", { name: "独立会话", exact: true }).locator("xpath=ancestor::section[1]");
  const independentRows = independent.locator('[data-sidebar-item]');
  const initialIndependent = await ids(independentRows);
  await drag(page, row(unbound[0].session_id), row(unbound[2].session_id));
  await expect.poll(async () => (await ids(independentRows)).slice(0, 3)).toEqual([unbound[0].session_id, unbound[2].session_id, unbound[1].session_id]);
  await sidebar.getByRole("button", { name: "恢复独立会话默认顺序" }).click();
  await page.getByRole("dialog", { name: "恢复独立会话的默认顺序？" }).getByRole("button", { name: "恢复默认", exact: true }).click();
  await expect.poll(() => ids(independentRows)).toEqual(initialIndependent);

  const pinnedRows = sidebar.getByRole("region", { name: "置顶会话" }).locator('[data-sidebar-item]');
  await drag(page, row(pins[1].session_id), row(pins[0].session_id));
  await expect.poll(() => ids(pinnedRows)).toEqual([pins[1].session_id, pins[0].session_id]);
  await expect.poll(async () => (await command("get_application_snapshot")).snapshot.value.pinned_sessions.map(s => s.session_id)).toEqual([pins[1].session_id, pins[0].session_id]);

  let failed = false;
  await page.route("**/commands", async route => {
    if (!failed && route.request().postData()?.includes('"move_sidebar_item"')) {
      failed = true;
      const body = route.request().postDataJSON();
      await route.fulfill({ status: 409, contentType: "application/json", body: JSON.stringify({ request_id: body.request_id, error: { code: "conflict", message: "changed" } }) });
    } else await route.continue();
  });
  await drag(page, row(unbound[0].session_id), row(unbound[2].session_id));
  await expect(sidebar.getByRole("alert")).toHaveText("列表已更新，请重试");
  await expect.poll(() => ids(independentRows)).toEqual(initialIndependent);
  const beforeCross = writes.length;
  await drag(page, row(unbound[0].session_id), row(pins[0].session_id));
  expect(writes).toHaveLength(beforeCross);
  await page.reload();
  await expect.poll(() => ids(pinnedRows)).toEqual([pins[1].session_id, pins[0].session_id]);
  expect(errors).toEqual([]);
  await page.screenshot({ path: info.outputPath("sidebar-order.png"), animations: "disabled" });
});

test("展开和收起项目时，后续项目行平滑移动", async ({ page }) => {
  const host = JSON.parse(process.env.EZ_ASSISTANT_E2E_BOOTSTRAP!) as { base_url: string; access_token: string };
  const headers = { ...compatibilityHeaders(), Authorization: `Bearer ${host.access_token}`, "Content-Type": "application/json" };
  async function command<T extends RuntimeCommand["type"]>(type: T, payload: object): Promise<Extract<RuntimeCommandResult, { type: T }>["payload"]> {
    const response = await fetch(`${host.base_url}/commands`, {
      method: "POST", headers,
      body: JSON.stringify({ request_id: crypto.randomUUID(), command: { scope: "runtime", payload: { type, payload } } }),
    });
    expect(response.ok).toBe(true);
    return (await response.json()).result.payload.payload;
  }
  const following = (await command("register_workspace", { label: "相邻项目", primary_directory: null, additional_directories: [] })).workspace;
  const folder = (await command("register_workspace", { label: "展开测试项目", primary_directory: null, additional_directories: [] })).workspace;
  for (let index = 0; index < 6; index++) {
    await command("create_session", { title: `展开测试会话 ${index}`, workspace_id: folder.workspace_id });
  }
  const login = await fetch(`${host.base_url}/auth/login`, { method: "POST", headers, body: JSON.stringify({ method: "desktop" }) });
  expect(login.ok).toBe(true);
  const { token } = await login.json() as { token: string };
  await page.emulateMedia({ reducedMotion: "no-preference" });
  await page.goto(`${host.base_url}/#token=${encodeURIComponent(token)}`);

  const toggle = page.getByRole("button", { name: folder.label, exact: true });
  await expect(toggle).toBeVisible();
  if (await toggle.getAttribute("aria-expanded") !== "true") await toggle.click();
  await expect(page.getByRole("button", { name: "展开测试会话 5", exact: true })).toBeVisible();

  for (const expanded of [true, false, true, false]) {
    await expect(toggle).toHaveAttribute("aria-expanded", String(expanded));
    const offsets = await toggle.evaluate(async (button, following_id) => {
      const row = document.querySelector<HTMLElement>(`[data-sidebar-item="${following_id}"]`)!;
      let scroll = row.parentElement!;
      while (getComputedStyle(scroll).overflowY !== "auto") scroll = scroll.parentElement!;
      const top = () => row.getBoundingClientRect().top + scroll.scrollTop;
      const samples = [top()];
      (button as HTMLButtonElement).click();
      const until = performance.now() + 360;
      while (performance.now() < until) {
        await new Promise(requestAnimationFrame);
        samples.push(top());
      }
      return samples;
    }, following.workspace_id);
    const direction = expanded ? -1 : 1;
    expect(direction * (offsets.at(-1)! - offsets[0])).toBeGreaterThan(100);
    const reverse = offsets.slice(1).map((top, index) => direction * (offsets[index] - top));
    expect(Math.max(...reverse)).toBeLessThanOrEqual(1);
  }
});

test("拖动悬停加载下一页并保持手势，触屏长按与短划滚动分离", async ({ page, browserName }, info) => {
  test.setTimeout(120_000);
  const host = JSON.parse(process.env.EZ_ASSISTANT_E2E_BOOTSTRAP!) as { base_url: string; access_token: string };
  const headers = { ...compatibilityHeaders(), Authorization: `Bearer ${host.access_token}`, "Content-Type": "application/json" };
  async function command(type: string, payload: object = {}) {
    const response = await fetch(`${host.base_url}/commands`, { method: "POST", headers, body: JSON.stringify({ request_id: crypto.randomUUID(), command: { scope: "runtime", payload: { type, payload } } }) });
    expect(response.ok).toBe(true);
    return (await response.json()).result.payload.payload;
  }
  const project = (await command("register_workspace", { label: "分页拖动项目", primary_directory: null, additional_directories: [] })).workspace;
  for (let i = 0; i < 24; i++) await command("create_session", { title: `分页排序 ${String(i).padStart(2, "0")}`, workspace_id: project.workspace_id });
  const login = await fetch(`${host.base_url}/auth/login`, { method: "POST", headers, body: JSON.stringify({ method: "desktop" }) });
  const { token } = await login.json();
  await page.emulateMedia({ reducedMotion: "reduce" });
  await page.goto(`${host.base_url}/#token=${encodeURIComponent(token)}`);
  const projectRow = page.locator(`[data-sidebar-item="${project.workspace_id}"]`);
  const button = projectRow.locator('[data-sidebar-handle]');
  await expect(button).toBeVisible();
  if (await button.getAttribute("aria-expanded") !== "true") await button.click();
  const section = projectRow.locator("xpath=ancestor::section[1]");
  const rows = section.locator('[data-sidebar-item]').filter({ has: page.locator('[data-navigation-action]') });
  await expect(rows).toHaveCount(10);
  const more = section.getByRole("button", { name: "加载更多", exact: true });
  await more.scrollIntoViewIfNeeded();
  const source = rows.nth(8);
  const sourceId = await source.getAttribute('data-sidebar-item');
  const a = (await source.boundingBox())!;
  const b = (await more.boundingBox())!;
  await page.mouse.move(a.x + 75, a.y + a.height / 2);
  await page.mouse.down();
  await page.waitForTimeout(150);
  await page.mouse.move(a.x + 75, a.y + a.height / 2 + 9);
  await page.mouse.move(b.x + b.width / 2, b.y + b.height / 2, { steps: 10 });
  await expect(rows).toHaveCount(20);
  await expect(page.locator('[data-sidebar-item][data-sidebar-dragging="true"]')).toHaveCount(1);
  const target = rows.nth(10);
  const c = (await target.boundingBox())!;
  await page.mouse.move(c.x + 75, c.y + 3, { steps: 5 });
  await page.mouse.up();
  await expect.poll(async () => (await command("list_sessions", { group: { type: "workspace", workspace_id: project.workspace_id }, limit: 30 })).sessions[9].session_id).toBe(sourceId);

  await expect(rows.first()).toHaveAttribute("aria-busy", "false");
  // 靠近同一滚动容器底边时自动滚动；Esc 仍取消当前手势。
  await rows.first().scrollIntoViewIfNeeded();
  const start = (await rows.first().boundingBox())!;
  const scroll = rows.first().locator("xpath=ancestor::div[contains(@class, 'session_scroll')]");
  const viewport = (await scroll.boundingBox())!;
  const scrollBefore = await scroll.evaluate(element => element.scrollTop);
  await page.mouse.move(start.x + 75, start.y + 12);
  await page.mouse.down();
  await page.waitForTimeout(150);
  await page.mouse.move(start.x + 75, start.y + 21);
  await page.mouse.move(start.x + 75, viewport.y + viewport.height - 4, { steps: 10 });
  await expect.poll(() => scroll.evaluate(element => element.scrollTop)).toBeGreaterThan(scrollBefore + 8);
  await page.keyboard.press("Escape");
  await page.mouse.up();

  // 真正的 Chromium 触屏输入；WebKit 本测试仍覆盖加载更多与持续拖动。
  if (browserName === "chromium") {
    const cdp = await page.context().newCDPSession(page);
    await cdp.send("Emulation.setTouchEmulationEnabled", { enabled: true, maxTouchPoints: 1 });
    await rows.first().scrollIntoViewIfNeeded();
    const first = (await rows.first().boundingBox())!;
    const firstId = await rows.first().getAttribute('data-sidebar-item');
    const secondId = await rows.nth(1).getAttribute('data-sidebar-item');
    const point = (rect: typeof first, y: number) => ({ x: rect.x + 75, y, id: 1 });
    // 长按开始前位移交还滚动，不发送排序。
    await cdp.send("Input.dispatchTouchEvent", { type: "touchStart", touchPoints: [point(first, first.y + 12)] });
    await cdp.send("Input.dispatchTouchEvent", { type: "touchMove", touchPoints: [point(first, first.y + 35)] });
    await cdp.send("Input.dispatchTouchEvent", { type: "touchEnd", touchPoints: [] });
    await expect(rows.first()).toHaveAttribute('data-sidebar-item', firstId!);
    await rows.first().scrollIntoViewIfNeeded();
    const from = (await rows.nth(1).boundingBox())!;
    const to = (await rows.first().boundingBox())!;
    await cdp.send("Input.dispatchTouchEvent", { type: "touchStart", touchPoints: [point(from, from.y + 12)] });
    await expect(page.locator('[data-sidebar-item][data-sidebar-dragging="true"]')).toHaveCount(1);
    await cdp.send("Input.dispatchTouchEvent", { type: "touchMove", touchPoints: [point(to, to.y + 3)] });
    await cdp.send("Input.dispatchTouchEvent", { type: "touchEnd", touchPoints: [] });
    await expect(rows.first()).toHaveAttribute('data-sidebar-item', secondId!);
    await cdp.detach();
  }
  await page.screenshot({ path: info.outputPath("sidebar-drag-more.png"), animations: "disabled" });
});
