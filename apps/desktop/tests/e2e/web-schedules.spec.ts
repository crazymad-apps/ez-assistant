import { readFile, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { compatibilityHeaders, type RuntimeCommand, type RuntimeCommandResult } from "@ez-assistant/protocol";
import { expect, test, type Locator, type Page } from "@playwright/test";

test.use({ timezoneId: "America/Los_Angeles" });

async function openSchedules(page: Page) {
  const host = JSON.parse(process.env.EZ_ASSISTANT_E2E_BOOTSTRAP!) as { base_url: string; access_token: string };
  const headers = { ...compatibilityHeaders(), Authorization: `Bearer ${host.access_token}`, "Content-Type": "application/json" };
  async function command<T extends RuntimeCommand["type"]>(
    type: T, payload: object,
  ): Promise<Extract<RuntimeCommandResult, { type: T }>["payload"]> {
    const response = await fetch(`${host.base_url}/commands`, {
      method: "POST", headers, body: JSON.stringify({
        request_id: crypto.randomUUID(), command: { scope: "runtime", payload: { type, payload } },
      }),
    });
    const body = await response.json();
    expect(response.ok, JSON.stringify(body)).toBe(true);
    return body.result.payload.payload;
  }

  const login = await fetch(`${host.base_url}/auth/login`, { method: "POST", headers, body: JSON.stringify({ method: "desktop" }) });
  expect(login.ok).toBe(true);
  const { token } = await login.json();
  await page.setViewportSize({ width: 1380, height: 900 });
  await page.emulateMedia({ reducedMotion: "reduce" });
  await page.goto(`${host.base_url}/#token=${encodeURIComponent(token)}`);
  const entry = page.getByRole("button", { name: "定时任务", exact: true });
  await expect(entry).toBeVisible();
  await entry.click();
  await expect(page.getByRole("heading", { name: "任务计划", exact: true })).toBeVisible();
  console.log(`F02 M4 fixture API ${host.base_url}`);
  return { command, base_url: host.base_url };
}

async function choose(page: Page, field: string, option: string) {
  await page.getByRole("button", { name: field, exact: true }).click();
  await page.getByRole("option").filter({ has: page.getByText(option, { exact: true }) }).click();
}

test("Agent 模型选择框沿用表单样式，选项面板跟随字段宽度", async ({ page }) => {
  await page.addInitScript(() => localStorage.setItem("ez-assistant:theme", "solarized_light"));
  await openSchedules(page);
  await expect(page.locator("html")).toHaveAttribute("data-theme", "solarized_light");
  await page.getByRole("button", { name: "新建计划" }).click();
  const field = (name: string) => page.getByRole("button", { name, exact: true });
  const agent = field("智能体");
  const model = field("模型");
  const approval = field("工具权限模式");
  const style = (field: Locator) => field.evaluate((element) => {
    const css = getComputedStyle(element);
    const icon = getComputedStyle(element.querySelector("svg")!);
    return [css.height, css.color, css.backgroundColor, css.borderColor, css.borderRadius, css.padding,
      css.fontFamily, css.fontSize, css.fontWeight, icon.color];
  });

  const resting = await style(agent);
  expect(await style(model)).toEqual(resting);
  expect(await style(approval)).toEqual(resting);
  await agent.hover();
  await expect.poll(() => style(agent)).not.toEqual(resting);
  const hovered = await style(agent);
  await model.hover();
  await expect.poll(() => style(model)).toEqual(hovered);
  await page.mouse.move(0, 0);
  await agent.focus();
  await page.keyboard.press("Tab");
  await expect(model).toBeFocused();
  await expect.poll(async () => (await style(model))[3]).not.toBe(resting[3]);
  const focused = await style(model);
  await page.keyboard.press("Shift+Tab");
  await expect(agent).toBeFocused();
  await expect.poll(() => style(agent)).toEqual(focused);

  await model.press("Enter");
  await expect(page.getByRole("menu", { name: "模型", exact: true })).toBeVisible();
  await page.keyboard.press("Escape");
  await expect(model).toBeFocused();

  for (const name of ["智能体", "模型", "工具权限模式"]) {
    await page.setViewportSize({ width: 1380, height: 900 });
    await field(name).click();
    const popup = page.getByRole(name === "模型" ? "menu" : "listbox", { name, exact: true });
    await expect(popup).toBeVisible();
    const assertWidth = async () => {
      await expect.poll(async () => {
        const popup_box = await popup.evaluate((element) => {
          const rect = element.closest('[data-overlay-region="floating"]')!.getBoundingClientRect();
          return { left: rect.left, width: rect.width, right: rect.right, viewport: window.innerWidth };
        });
        const trigger_box = (await field(name).boundingBox())!;
        return Math.max(Math.abs(popup_box.width - trigger_box.width), Math.abs(popup_box.left - trigger_box.x),
          popup_box.right - popup_box.viewport);
      }).toBeLessThanOrEqual(1);
    };
    await assertWidth();
    await page.setViewportSize({ width: 780, height: 900 });
    await assertWidth();
    await page.keyboard.press("Escape");
    await expect(popup).toBeHidden();
  }
});

test("主题日期和时间选择支持键盘、有效性校验及保存回填", async ({ page }) => {
  await page.addInitScript(() => localStorage.setItem("ez-assistant:theme", "solarized_light"));
  const { command } = await openSchedules(page);
  await page.setViewportSize({ width: 580, height: 520 });
  await page.getByRole("button", { name: "新建计划" }).click();
  const name = "一次性日期与时间夹具";
  await page.getByLabel("名称", { exact: true }).fill(name);
  await page.getByLabel("任务指令", { exact: true }).fill("仅验证日期与时间字段");
  await choose(page, "执行频率", "一次");
  const date = page.getByRole("button", { name: /^日期 / });
  await expect(date).toHaveText("选择日期");
  await date.press("Enter");
  const calendar = page.getByRole("dialog", { name: "选择日期" });
  await expect(calendar.locator("th").first()).toHaveText("一");
  await expect.poll(() => calendar.evaluate((node) => {
    const rect = node.getBoundingClientRect();
    return rect.left >= 0 && rect.top >= 0 && rect.right <= innerWidth && rect.bottom <= innerHeight;
  })).toBe(true);

  const next_month = calendar.getByRole("button", { name: "前往下个月" });
  const previous_month = calendar.getByRole("button", { name: "前往上个月" });
  const month_grid = calendar.getByRole("grid");
  const original_month = await month_grid.getAttribute("aria-label");
  await calendar.locator('[data-day] button[tabindex="0"]').focus();
  // 重现 WKWebView 点击翻月箭头时，将焦点暂时交给日历浮层以外节点的行为。
  await next_month.evaluate((arrow) => {
    arrow.addEventListener("pointerdown", () => {
      document.querySelector<HTMLElement>('h1[tabindex="-1"]')?.focus({ preventScroll: true });
    }, { once: true });
  });
  await next_month.click();
  await expect(date).toHaveAttribute("aria-expanded", "true");
  await expect(month_grid).not.toHaveAttribute("aria-label", original_month!);
  const first_next_month = await month_grid.getAttribute("aria-label");
  await next_month.click();
  await expect(month_grid).not.toHaveAttribute("aria-label", first_next_month!);
  await previous_month.click();
  await expect(month_grid).toHaveAttribute("aria-label", first_next_month!);
  await expect(previous_month).toBeFocused();
  await expect(date).toHaveText("选择日期");

  // 选下个月中的日历日，再用方向键移动一天，覆盖从日历到协议字符串的往返。
  const day = calendar.locator('[data-day]:not([data-outside]) button').nth(10);
  await day.focus();
  await page.keyboard.press("ArrowRight");
  const next = calendar.locator('button:focus').locator("..");
  const selected = await next.getAttribute("data-day");
  expect(selected).toMatch(/^\d{4}-\d{2}-\d{2}$/);
  await page.keyboard.press("Enter");
  await expect(calendar).toBeHidden();
  await expect(date).toHaveText(selected!);
  await expect(date).toBeFocused();

  const time = page.getByLabel("时间（设备当地时间）", { exact: true });
  await time.fill("24:00");
  await time.press("Tab");
  await expect(time).toHaveAttribute("aria-invalid", "true");
  const save = page.getByRole("button", { name: "保存计划" });
  await expect(save).toBeDisabled();
  await time.fill("00:00");
  await time.press("ArrowDown");
  const time_picker = page.getByRole("dialog", { name: "选择时间" });
  await expect(time_picker.getByRole("radio", { name: "0时", exact: true })).toBeFocused();
  await time_picker.getByRole("radio", { name: "23时", exact: true }).check();
  await time_picker.getByRole("radio", { name: "59分", exact: true }).check();
  await time_picker.getByRole("button", { name: "确定" }).click();
  await expect(time_picker).toBeHidden();
  await expect(time).toHaveValue("23:59");
  const time_button = page.getByRole("button", { name: "选择时间" });
  await expect(time_button).toBeFocused();
  await time_button.click();
  await expect(time_picker.getByRole("radio", { name: "23时", exact: true })).toBeFocused();
  await page.keyboard.press("Escape");
  await expect(time_picker).toBeHidden();
  await expect(time_button).toBeFocused();

  const rule = { type: "once", date: selected, time: "23:59" };
  const preview = await command("preview_schedule", { rule });
  expect(preview.next_run_at?.slice(0, 16)).toBe(`${selected}T23:59`);
  await expect(page.getByText(`下次触发 ${selected} 23:59`, { exact: true })).toBeVisible();
  await save.click();
  await expect(page.getByRole("heading", { name, exact: true })).toBeVisible();
  const plans = await command("list_schedules", {});
  expect(plans.schedules.find((plan) => plan.settings.name === name)?.settings.rule).toEqual(rule);
  await page.getByRole("button", { name: "编辑", exact: true }).click();
  await expect(date).toHaveText(selected!);
  await expect(time).toHaveValue("23:59");

  await date.press("Enter");
  await expect(calendar).toBeVisible();
  await previous_month.click();
  await page.keyboard.press("Escape");
  await expect(calendar).toBeHidden();
  await expect(date).toBeFocused();
  await date.press("Enter");
  const focused_day = calendar.locator(`[data-day="${selected}"] button[tabindex="0"]`);
  await expect(focused_day).toBeFocused();
  await page.keyboard.press("Tab");
  await expect(calendar).toBeHidden();
});

test("计划编辑、聊天授权、手动脚本、逆序分页和删除", async ({ page }) => {
  test.skip(process.platform === "win32", "脚本夹具使用 POSIX 命令");
  test.setTimeout(80_000);
  const { command, base_url } = await openSchedules(page);
  const name = "计划界面 · 脚本";
  const script = "printf '标准输出夹具\\n'; printf '标准错误夹具\\n' >&2";
  await page.getByRole("button", { name: "新建计划" }).click();
  await expect(page.getByLabel("任务指令", { exact: true })).toBeVisible();
  await choose(page, "任务类型", "脚本任务");
  await expect(page.getByLabel("工具权限模式", { exact: true })).toHaveCount(0);
  await page.getByLabel("名称", { exact: true }).fill(name);
  await page.getByLabel("Shell 脚本", { exact: true }).fill(script);
  await choose(page, "执行频率", "每周");
  await page.getByLabel("周一", { exact: true }).check();
  await page.getByLabel("周五", { exact: true }).check();
  await page.getByLabel("将执行结果返回主控会话").uncheck();
  const { next_run_at } = await command("preview_schedule", {
    rule: { type: "weekly", time: "09:00", weekdays: ["monday", "friday"] },
  });
  expect(next_run_at).not.toBeNull();
  await expect(page.getByText(`下次触发 ${next_run_at!.slice(0, 16).replace("T", " ")}`, { exact: true })).toBeVisible();
  await page.getByRole("button", { name: "保存计划" }).click();
  await expect(page.getByRole("heading", { name })).toBeVisible();
  const nav = page.getByRole("navigation", { name: "定时任务导航" });
  await expect(nav.getByRole("button", { name: "返回计划列表" })).toBeVisible();
  await expect(page.getByRole("button", { name: "返回计划列表" })).toHaveCount(1);
  await nav.getByRole("button", { name: "返回计划列表" }).click();
  await expect(page.getByRole("heading", { name: "任务计划" })).toBeFocused();
  await expect(nav.getByRole("button", { name: "返回会话" })).toBeVisible();
  await page.getByRole("button", { name: new RegExp(name) }).click();
  await expect(page.getByRole("heading", { name })).toBeVisible();
  const definition = page.getByRole("region", { name: "计划配置" });
  await expect(definition.getByText("周一、周五", { exact: true })).toBeVisible();
  await expect(definition.locator("pre")).toHaveText(script);
  const schedule = (await command("list_schedules", {})).schedules.find((item) => item.settings.name === name)!;
  expect(schedule.settings.action).toEqual({ type: "script", command: script });
  expect(schedule.settings.rule).toMatchObject({ type: "weekly", weekdays: ["monday", "friday"] });
  expect(schedule.settings.report_to_controller).toBe(false);
  const schedule_id = schedule.schedule_id;

  const { session } = await command("create_session", { title: "定时任务管理对话" });
  const session_id = session.session_id;
  await command("set_session_approval_mode", { session_id, approval_mode: "ask" });
  const { input_id } = await command("submit_input", {
    session_id, message: `F02_CHAT_SCHEDULE ${schedule_id} 尝试暂停计划`, variant: "plan",
  });
  await expect.poll(async () => (await command("list_runs", { session_id, input_id })).runs[0]?.status).toBe("completed");
  expect((await command("get_schedule", { schedule_id })).settings.enabled).toBe(true);
  expect((await command("list_pending_approvals", { session_id })).approvals).toEqual([]);
  await command("submit_input", { session_id, message: `F02_CHAT_SCHEDULE ${schedule_id} 请暂停计划`, variant: "build" });
  await expect.poll(async () => (await command("list_pending_approvals", { session_id })).approvals.length).toBe(1);
  const [approval] = (await command("list_pending_approvals", { session_id })).approvals;
  expect(approval.subject).toEqual({ type: "general", tool_name: "manage_schedules" });
  expect(approval.available_decisions).toEqual(["allow_once", "deny"]);
  expect((await command("get_schedule", { schedule_id })).settings.enabled).toBe(true);
  await command("decide_approval", { session_id, approval_id: approval.approval_id, decision: "allow_once" });
  await expect(page.getByRole("button", { name: "启用", exact: true })).toBeVisible();
  await expect(page.getByText("已暂停", { exact: true })).toBeVisible();

  let lost_reply = false;
  await page.route("**/commands", async (route) => {
    const command = route.request().postDataJSON()?.command?.payload;
    if (lost_reply || command?.type !== "set_schedule_enabled" || !command.payload.enabled) {
      return route.continue();
    }
    lost_reply = true;
    const response = await route.fetch();
    expect(response.ok()).toBe(true);
    return route.fulfill({ status: 502, json: { error: { code: "transport_error", message: "测试回执丢失" } } });
  });
  await page.getByRole("button", { name: "启用", exact: true }).click();
  await expect(page.getByText("已刷新，请核对最新状态。")).toBeVisible();
  expect(lost_reply).toBe(true);
  await page.getByRole("button", { name: "暂停", exact: true }).click();
  await expect(page.getByRole("button", { name: "启用", exact: true })).toBeVisible();

  await page.getByRole("button", { name: "立即执行" }).click();
  const records = page.getByRole("region", { name: "执行记录" });
  const entries = records.getByRole("button", { name: /手动/ });
  const result = records.getByRole("region", { name: "执行结果" });
  await expect(result.getByLabel("标准输出")).toHaveText("标准输出夹具\n");
  await expect(entries.first()).toHaveAttribute("aria-expanded", "true");
  await expect(entries.first().locator("..").getByRole("region", { name: "执行结果" })).toBeVisible();
  await expect(result.getByLabel("标准错误")).toHaveText("标准错误夹具\n");
  await expect(result.getByText("退出码 0")).toBeVisible();
  await expect(result.getByText("仅保存在执行记录")).toBeVisible();
  await expect(result.getByRole("button", { name: "前往助理会话" })).toHaveCount(0);

  for (let index = 0; index < 10; index++) {
    const accepted = await command("run_schedule_now", { schedule_id, idempotency_key: crypto.randomUUID() });
    expect(accepted.type).toBe("accepted");
    await expect.poll(async () => (await command("get_schedule_execution", {
      execution_id: accepted.execution.execution_id,
    })).status).toBe("succeeded");
  }
  await page.getByRole("button", { name: "刷新执行记录" }).click();
  await expect(page.getByRole("navigation", { name: "执行记录分页" })).toContainText("1 / 2");
  await entries.first().click();
  await expect(entries.first()).toHaveAttribute("aria-expanded", "true");
  await expect(entries.first().locator("..").getByRole("region", { name: "执行结果" })).toBeVisible();
  await entries.nth(1).focus();
  await page.keyboard.press("Enter");
  await expect(entries.first()).toHaveAttribute("aria-expanded", "false");
  await expect(entries.nth(1)).toHaveAttribute("aria-expanded", "true");
  await expect(entries.nth(1).locator("..").getByRole("region", { name: "执行结果" })).toBeVisible();
  await expect(result).toHaveCount(1);
  await entries.nth(1).click();
  await expect(entries.nth(1)).toHaveAttribute("aria-expanded", "false");
  await expect(result).toHaveCount(0);
  const history = await command("list_schedule_executions", { schedule_id, page: { offset: 0, limit: 10 } });
  expect(history.page.total).toBe(11);
  expect(history.executions[0].accepted_at_ms).toBeGreaterThan(history.executions[9].accepted_at_ms);
  await page.getByRole("button", { name: "下一页执行记录" }).click();
  await expect(page.getByRole("navigation", { name: "执行记录分页" })).toContainText("2 / 2");
  await expect(page.getByRole("region", { name: "执行记录" }).getByRole("listitem")).toHaveCount(1);

  await page.getByRole("button", { name: "编辑", exact: true }).click();
  await expect(nav.getByRole("button", { name: "返回计划详情" })).toBeVisible();
  await nav.getByRole("button", { name: "返回计划详情" }).click();
  await expect(page.getByRole("heading", { name })).toBeFocused();
  await page.getByRole("button", { name: "编辑", exact: true }).click();
  await choose(page, "任务类型", "Agent 任务");
  const instructions = "## 离线 Agent 计划\n\n- 汇总 `report.md`";
  await page.getByLabel("任务指令", { exact: true }).fill(instructions);
  await expect(page.getByRole("button", { name: "工具权限模式" })).toBeVisible();
  await page.getByRole("button", { name: "保存计划" }).click();
  await expect(definition.getByRole("heading", { name: "离线 Agent 计划" })).toBeVisible();
  await expect(definition.getByRole("listitem")).toHaveCount(1);
  await expect(definition.locator("code")).toHaveText("report.md");
  await expect(definition.getByRole("textbox")).toHaveCount(0);
  expect((await command("get_schedule", { schedule_id })).settings.action).toEqual({
    type: "agent", instructions, approval_mode: "ask",
  });

  const screenshot = process.env.EZ_ASSISTANT_E2E_SCHEDULE_SCREENSHOT;
  if (screenshot) await page.screenshot({ path: screenshot });
  await page.getByRole("button", { name: "删除计划", exact: true }).click();
  const dialog = page.getByRole("dialog");
  await expect(dialog.getByRole("button", { name: "取消" })).toBeFocused();
  await page.keyboard.press("Escape");
  await expect(dialog).toHaveCount(0);
  await page.getByRole("button", { name: "删除计划", exact: true }).click();
  await dialog.getByRole("button", { name: "删除计划", exact: true }).click();
  await expect(page.getByRole("heading", { name: "任务计划" })).toBeVisible();
  await expect(page.getByRole("button", { name: new RegExp(name) })).toHaveCount(0);
  expect((await command("list_schedules", {})).schedules.some((item) => item.schedule_id === schedule_id)).toBe(false);
  console.log(`F02 M4 fixture API ${base_url}, deleted plan ${schedule_id} and 11 executions`);
});

test("主控整理 Agent 结果，原文默认折叠并保留归档下载", async ({ page }) => {
  const { command } = await openSchedules(page);
  const dirname = process.env.EZ_ASSISTANT_E2E_NEW_WORKSPACE!;
  await writeFile(join(dirname, "schedule-report.txt"), "F02 归档文件正文\n");
  await writeFile(join(dirname, "schedule-details.txt"), "F02 明细文件正文\n");
  await command("register_workspace", {
    label: "定时任务文件项目", primary_directory: dirname, additional_directories: [],
  });
  await page.getByRole("button", { name: "新建计划" }).click();
  await page.getByLabel("名称", { exact: true }).fill("计划界面 · Agent");
  await page.getByLabel("任务指令", { exact: true }).fill("F02_AGENT_JOB");
  await choose(page, "关联项目", "定时任务文件项目");
  await expect(page.getByLabel("将执行结果返回主控会话")).toBeChecked();
  await page.getByRole("button", { name: "保存计划" }).click();
  await page.getByRole("button", { name: "立即执行" }).click();
  const result = page.getByRole("region", { name: "执行结果" });
  await expect(result.getByText("Agent 归档汇报", { exact: false })).toBeVisible({ timeout: 15_000 });
  const file = result.getByRole("button", { name: "定时归档文件" });
  await expect(file).toBeVisible();
  const download_event = page.waitForEvent("download");
  await file.click();
  const download = await download_event;
  expect(download.suggestedFilename()).toBe("schedule-report.txt");
  expect(await readFile((await download.path())!, "utf8")).toBe("F02 归档文件正文\n");
  const bundle_event = page.waitForEvent("download");
  await result.getByRole("button", { name: "下载全部文件" }).click();
  const bundle = await bundle_event;
  expect(bundle.suggestedFilename()).toBe("schedule-results.zip");
  const zip = await readFile((await bundle.path())!);
  expect(zip.subarray(0, 4)).toEqual(Buffer.from("504b0304", "hex"));
  expect(zip.includes(Buffer.from("F02 归档文件正文"))).toBe(true);
  expect(zip.includes(Buffer.from("F02 明细文件正文"))).toBe(true);
  await result.getByRole("button", { name: "前往助理会话" }).click();
  await expect(page.getByRole("heading", { name: "计划界面 · Agent" })).toHaveCount(0);
  const source = page.locator('[data-source="schedule_report"]');
  const raw = source.getByText("Agent 归档汇报", { exact: true }).and(source.locator('[data-streamdown="strong"]'));
  const files = source.getByRole("button", { name: "定时归档文件" }).locator("..");
  const expectFileSpacing = async () => {
    await expect(files).toHaveCSS("padding", "6px");
    await expect(files).toHaveCSS("border-top-width", "1px");
  };
  await expect(source.locator("details")).not.toHaveAttribute("open");
  await expect(raw).toBeHidden();
  await expectFileSpacing();
  await expect(page.getByText("助理已整理定时任务结果，两份文件可以直接下载。", { exact: true })).toBeVisible();
  await source.locator("summary").click();
  await expect(raw).toBeVisible();
  await expect(raw).toHaveCSS("font-weight", "600");
  await expect(source.getByRole("listitem")).toHaveCount(2);
  await expect(source.locator("code").getByText("schedule-report.txt", { exact: true })).toBeVisible();
  await expectFileSpacing();
  await source.locator("summary").click();
  await expect(raw).toBeHidden();
  await expect(page.getByRole("button", { name: "定时归档文件", exact: false }).first()).toBeVisible();
  const plan = (await command("list_schedules", {})).schedules.find((item) => item.settings.name === "计划界面 · Agent")!;
  await command("delete_schedule", { schedule_id: plan.schedule_id });
  const delivered = page.waitForEvent("download");
  await page.getByRole("button", { name: "下载全部文件", exact: true }).click();
  const retained = await delivered;
  const retained_zip = await readFile((await retained.path())!);
  expect(retained_zip.includes(Buffer.from("F02 归档文件正文"))).toBe(true);
  expect(retained_zip.includes(Buffer.from("F02 明细文件正文"))).toBe(true);

  const layout = (scroll: Locator) => scroll.evaluate((node) => [
    getComputedStyle(node).padding, getComputedStyle(node.firstElementChild!).maxWidth,
  ]);
  const conversation = page.getByLabel("消息列表", { exact: true });
  await expect.poll(() => layout(conversation)).toEqual(["24px 16px 28px", "820px"]);
  const wide = await layout(conversation);
  await page.setViewportSize({ width: 700, height: 900 });
  await expect.poll(() => layout(conversation)).toEqual(["18px 16px 28px", "820px"]);
  const narrow = await layout(conversation);
  await page.setViewportSize({ width: 1380, height: 900 });
  await page.getByRole("button", { name: "定时任务", exact: true }).click();
  const schedule_scroll = page.getByRole("heading", { name: "任务计划" }).locator("xpath=ancestor::header/../..");
  await expect.poll(() => layout(schedule_scroll)).toEqual(wide);
  await page.setViewportSize({ width: 700, height: 900 });
  await expect.poll(() => layout(schedule_scroll)).toEqual(narrow);
});
