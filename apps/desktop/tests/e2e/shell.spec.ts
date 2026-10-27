import { compatibilityHeaders } from "@ez-assistant/protocol";
import { expect, test } from "@playwright/test";
import type { Page } from "@playwright/test";
import { execFileSync } from "node:child_process";
import { mkdtempSync, statSync } from "node:fs";
import { basename, join, resolve } from "node:path";
import { expectMcpAcceptance } from "./mcp-acceptance";

test("loads real workspaces and sessions from the temporary Runtime Host", async ({ page }) => {
  test.setTimeout(60_000);
  page.on("console", (message) => { if (message.type() === "error" || message.text().includes("DOTS")) console.log("entry-renderer:", message.text()); });
  const bootstrap = process.env.EZ_ASSISTANT_E2E_BOOTSTRAP;
  if (!bootstrap) {
    throw new Error("temporary Runtime bootstrap is missing");
  }
  const new_workspace = process.env.EZ_ASSISTANT_E2E_NEW_WORKSPACE;
  if (!new_workspace) {
    throw new Error("temporary workspace picker fixture is missing");
  }
  const runtime_bootstrap = JSON.parse(bootstrap) as {
    readonly access_token: string;
    readonly base_url: string;
  };
  const retired_demo = await fetch(`${runtime_bootstrap.base_url}/demo`, {
    headers: { ...compatibilityHeaders(), Authorization: `Bearer ${runtime_bootstrap.access_token}` },
  });
  expect(retired_demo.status).toBe(404);
  const compatibility_headers = compatibilityHeaders();
  await page.addInitScript(({ serialized_bootstrap, workspace_directory, compatibility_headers }) => {
    let callback_id = 0;
    Object.defineProperty(globalThis, "isTauri", { value: true });
    Object.defineProperty(globalThis, "__TAURI_EVENT_PLUGIN_INTERNALS__", {
      value: { unregisterListener() {} },
    });
    Object.defineProperty(globalThis, "__TAURI_INTERNALS__", {
      value: {
        transformCallback() {
          callback_id += 1;
          return callback_id;
        },
        async invoke(command: string, args?: unknown) {
          if (command === "plugin:event|listen") {
            callback_id += 1;
            return Promise.resolve(callback_id);
          }
          if (command === "plugin:event|unlisten") {
            return Promise.resolve();
          }
          if (command === "begin_runtime_connection") return "e2e-binding";
          if (command === "connect_runtime_target") return { bootstrap: JSON.parse(serialized_bootstrap), warning: null };
          if (command === "refresh_runtime_connection") return JSON.parse(serialized_bootstrap);
          if (command === "shutdown_user_terminals" || command === "resume_user_terminals") return;
          if (command === "bootstrap_runtime") {
            return Promise.resolve(JSON.parse(serialized_bootstrap));
          }
          if (command === "desktop_platform") {
            return Promise.resolve("unsupported");
          }
          if (command === "take_pending_desktop_lifecycle_intent") {
            return Promise.resolve(null);
          }
          if (command === "update_native_runtime_state") {
            return Promise.resolve();
          }
          if (command === "load_desktop_preferences") {
            return Promise.resolve({
              left_sidebar_open: true,
              right_sidebar_open: true,
              left_sidebar_width: 286,
              right_sidebar_width: 380,
              expanded_workspace_ids: null,
              close_behavior: "hide_to_tray",
              default_approval_mode: "ask",
              last_model_selection: null,
            });
          }
          if (command === "save_desktop_preferences") {
            return Promise.resolve();
          }
          if (command === "layout_resource_browser") {
            // 模拟原生 UI 线程异步完成，让错误的滚动显隐能够被页面回归检测到。
            return new Promise<void>((resolve) => setTimeout(resolve, 40));
          }
          if (command === "choose_workspace_directory") {
            return Promise.resolve(workspace_directory);
          }
          if (command === "manage_local_host") {
            const runtime = JSON.parse(serialized_bootstrap) as {
              readonly access_token: string;
              readonly base_url: string;
            };
            const response = await fetch(`${runtime.base_url}/commands`, {
              method: "POST",
              headers: { ...compatibility_headers, Authorization: `Bearer ${runtime.access_token}`, "Content-Type": "application/json" },
              body: JSON.stringify({ request_id: `e2e-host-access-${++callback_id}`, command: { scope: "host_access", payload: (args as { command: object }).command } }),
            });
            const result = await response.json() as { result?: { payload?: unknown }; error?: { message?: string } };
            if (!response.ok || !result.result) throw new Error(result.error?.message ?? `Host access command failed: ${response.status}`);
            return result.result.payload;
          }
          if (command === "materialize_new_session") {
            const request = args as { readonly manifest: object };
            const runtime = JSON.parse(serialized_bootstrap) as {
              readonly access_token: string;
              readonly base_url: string;
            };
            const body = new FormData();
            body.append("manifest", JSON.stringify(request.manifest));
            const response = await fetch(`${runtime.base_url}/session-materializations`, {
              method: "POST",
              headers: { ...compatibility_headers, Authorization: `Bearer ${runtime.access_token}` },
              body,
            });
            const payload = await response.json() as { readonly error?: { readonly message?: string } };
            if (!response.ok) {
              throw new Error(payload.error?.message ?? `materialization failed: ${response.status}`);
            }
            return payload;
          }
          return Promise.reject(new Error(`Unexpected desktop command: ${command}`));
        },
      },
    });
  }, { serialized_bootstrap: bootstrap, workspace_directory: new_workspace, compatibility_headers });

  await page.goto("/");
  await expect(page).toHaveTitle("EZ Assistant");
  await expect(page.locator("[data-runtime-entry]")).toBeVisible();
  await expect(page.locator("[data-dots-state='ready'] canvas")).toHaveCount(1);
  await page.screenshot({ path: test.info().outputPath("desktop-entry.png"), animations: "disabled" });
  await expect(page.getByText("E2E Workspace", { exact: true })).toHaveCount(0);
  await page.getByRole("button", { name: "进入项目" }).click();
  const transition = page.locator("[data-workspace-transition]");
  await expect(transition).toHaveAttribute("data-workspace-transition", "complete");
  await expect(transition.locator("canvas")).toHaveCount(0);
  await page.screenshot({ path: test.info().outputPath("desktop-workspace.png") });
  await expect(page.getByRole("textbox", { name: "输入消息" })).toBeFocused();
  await expect(page.locator("[data-runtime-entry]")).toHaveCount(0);
  await expect(page.getByRole("navigation", { name: "会话导航操作" })).toBeVisible();
  await expect(page.getByRole("button", { name: "新建会话" })).toBeVisible();
  await expect(page.getByRole("tabpanel", { name: "当前上下文" })).toBeVisible();
  await expect(page.getByText("Runtime 已连接")).toBeVisible();
  await expectResponsiveLayouts(page);
  await expectSettingsScrollWithoutFlicker(page);
  await expectProviderConnectionForm(page);
  await expectDeviceGatewayManagement(page);
  const navigation = page.getByRole("complementary", { name: "会话导航" });
  const session_header = page.locator('header[aria-label="会话标题栏"]');
  const seeded_session = navigation.getByRole("button", { name: /^M2 临时会话(?:\s|$)/ });
  await expect(seeded_session).toBeVisible();
  await expect(session_header.getByRole("button", { name: "M2 临时会话" })).toBeVisible();
  const seeded_row = seeded_session.locator("..");
  const pinned_action = seeded_row.getByRole("button", { name: "置顶 M2 临时会话" });
  await expect(pinned_action.locator("svg")).toHaveAttribute("data-icon", "pin-outline");
  await page.mouse.move(700, 400);
  await expect.poll(() => seeded_row.locator("time").evaluate((element) => getComputedStyle(element).opacity)).toBe("1");
  expect(await seeded_row.evaluate((row) => row.getBoundingClientRect().right - row.querySelector("time")!.getBoundingClientRect().right))
    .toBeGreaterThanOrEqual(8);
  await expect.poll(() => pinned_action.evaluate((element) => getComputedStyle(element).opacity)).toBe("0");
  await seeded_session.hover();
  await expect.poll(() => seeded_row.locator("time").evaluate((element) => getComputedStyle(element).opacity)).toBe("0");
  await expect.poll(() => pinned_action.evaluate((element) => getComputedStyle(element).opacity)).toBe("1");
  await page.screenshot({ path: test.info().outputPath("sidebar-unpinned-hover.png"), animations: "disabled" });
  await pinned_action.click();
  const pinned_section = navigation.getByRole("region", { name: "置顶会话" });
  await expect(pinned_section.getByRole("button", { name: /^M2 临时会话/ })).toBeVisible();
  await page.mouse.move(700, 400);
  await page.screenshot({ path: test.info().outputPath("sidebar-pinned-default.png"), animations: "disabled" });
  await pinned_section.getByRole("button", { name: /^M2 临时会话/ }).hover();
  await page.screenshot({ path: test.info().outputPath("sidebar-pinned-hover.png"), animations: "disabled" });
  const unpin_action = pinned_section.getByRole("button", { name: "取消置顶 M2 临时会话" });
  await expect(unpin_action.locator("svg")).toHaveAttribute("data-icon", "pin");
  await unpin_action.click();
  await expect(pinned_section).toHaveCount(0);
  const workspace_row = navigation.getByRole("button", { name: "E2E Workspace", exact: true }).locator("..");
  const workspace_action = workspace_row.getByRole("button", { name: "E2E Workspace 项目操作" });
  await page.mouse.move(700, 400);
  await expect.poll(() => workspace_action.evaluate((element) => getComputedStyle(element).opacity)).toBe("0");
  await workspace_row.hover();
  await expect.poll(() => workspace_action.evaluate((element) => getComputedStyle(element).opacity)).toBe("1");

  await navigation.getByRole("button", { name: "添加项目" }).click();
  const create_workspace_dialog = page.getByRole("dialog", { name: "新建项目" });
  await expect(create_workspace_dialog.getByRole("textbox", { name: /项目名称/ })).toHaveValue("");
  await create_workspace_dialog.getByRole("button", { name: "添加目录" }).click();
  await expect(create_workspace_dialog.getByRole("textbox", { name: /项目名称/ }))
    .toHaveValue(basename(new_workspace));
  await create_workspace_dialog.getByRole("button", { name: "创建" }).click();
  await expect(create_workspace_dialog).toBeHidden();
  await expect(navigation.getByText(basename(new_workspace), { exact: true })).toBeVisible();
  await expect(session_header.getByRole("button", { name: "新对话" })).toHaveCount(0);
  await expectDraftComposerLayout(page);
  await expect(page.getByRole("button", { name: "清空项目选择" })).toBeVisible();
  await page.getByRole("button", { name: "新会话项目" }).click();
  const workspace_picker = page.getByRole("dialog", { name: "选择新会话项目" });
  await expect(workspace_picker).toBeVisible();
  const quick_action_height = (await navigation.getByRole("button", { name: "新建会话" }).boundingBox())!.height;
  expect((await workspace_picker.getByRole("option").first().boundingBox())!.height).toBe(quick_action_height);
  expect((await workspace_picker.getByRole("button", { name: "新建项目" }).boundingBox())!.height).toBe(quick_action_height);
  await expect(workspace_picker).not.toContainText(new_workspace);
  await expect(workspace_picker.getByRole("option", { name: "无项目" })).toHaveCount(0);
  await workspace_picker.getByRole("combobox", { name: "搜索项目" }).fill(basename(new_workspace));
  await expect(workspace_picker.getByRole("option", { name: new RegExp(basename(new_workspace)) })).toBeVisible();
  await page.screenshot({ path: test.info().outputPath("new-session-workspace-picker.png"), animations: "disabled" });
  await page.keyboard.press("Escape");
  await expect(workspace_picker).toBeHidden();
  await page.getByRole("button", { name: "清空项目选择" }).click();
  await expect(page.getByRole("button", { name: "新会话项目" })).toContainText("选择项目");
  await page.getByRole("button", { name: "新会话项目" }).click();
  await workspace_picker.getByRole("option", { name: new RegExp(basename(new_workspace)) }).click();
  await expect(page.getByRole("button", { name: "清空项目选择" })).toBeVisible();
  await page.screenshot({ path: test.info().outputPath("new-session-workspace-drawer.png"), animations: "disabled" });
  await page.getByRole("button", { name: "模型设置" }).click();
  const default_model_option = page.getByRole("menuitemradio", { name: "默认模型" });
  await expect(default_model_option).toBeVisible();
  expect((await default_model_option.boundingBox())!.height).toBe(quick_action_height);
  await page.screenshot({ path: test.info().outputPath("new-session-model-menu.png"), animations: "disabled" });
  await page.keyboard.press("Escape");
  await seeded_session.click();
  await expect(session_header.getByRole("button", { name: "M2 临时会话" })).toBeVisible();

  await navigation.getByRole("button", { name: "搜索会话" }).click();
  const session_search = page.getByRole("dialog", { name: "搜索会话" });
  await expect(session_search).toBeVisible();
  await expect(navigation.getByRole("searchbox", { name: "搜索会话名称" })).toHaveCount(0);
  const search_input = session_search.getByRole("searchbox", { name: "搜索会话名称" });
  await search_input.fill("M2 临时");
  await expect(session_search.getByRole("button", { name: /M2 临时会话/ })).toBeVisible();
  await page.screenshot({ path: test.info().outputPath("session-search-dialog.png"), animations: "disabled" });
  await session_search.getByRole("button", { name: "关闭搜索" }).click();

  const workspace_menu_trigger = navigation.getByRole("button", { name: /项目操作/ }).first();
  await workspace_menu_trigger.click();
  await expect(page.getByRole("menu", { name: /项目操作/ })).toBeVisible();
  await expect(page.getByRole("menuitem", { name: "在此新建会话" })).toBeVisible();
  await expect(page.getByRole("menuitem", { name: "打开工作目录" })).toBeVisible();
  await expect(page.getByRole("menuitem", { name: "复制目录路径" })).toBeVisible();
  await expect(page.getByRole("menuitem", { name: /恢复已移除项目/ })).toHaveCount(0);
  await expect(page.getByRole("menuitem", { name: "移除项目…", exact: true })).toBeVisible();
  await page.keyboard.press("Escape");
  await expect(page.getByRole("menu", { name: /项目操作/ })).toBeHidden();

  const workspace_name = await seeded_session
    .locator("xpath=ancestor::section[1]")
    .locator('button[aria-expanded]')
    .first()
    .innerText();
  const workspace_toggle = navigation.getByRole("button", {
    name: workspace_name,
    exact: true,
  });
  await workspace_toggle.click();
  await expect(seeded_session).toBeHidden();
  await workspace_toggle.click();
  await expect(seeded_session).toBeVisible();

  await navigation.getByRole("button", { name: "E2E Workspace 项目操作" }).click();
  await page.getByRole("menuitem", { name: "在此新建会话" }).click();
  await expect(session_header.getByRole("button", { name: "新对话" })).toHaveCount(0);
  await expectDraftComposerLayout(page);
  const source_composer = page.getByRole("textbox", { name: "输入消息" });
  await selectMcpFixture(page);
  await source_composer.fill("SOURCE_CASE");
  const materialization_response = page.waitForResponse((response) =>
    response.url().endsWith("/session-materializations") && response.request().method() === "POST"
  );
  await source_composer.press("Enter");
  const materialized = await materialization_response;
  expect(materialized.ok(), await materialized.text()).toBe(true);
  await expect(source_composer).toHaveValue("");
  await expect(session_header.getByRole("button", { name: "SOURCE_CASE", exact: true })).toBeVisible();
  await expect(page.getByText("离线回复：DEFAULT_CASE", { exact: true })).toBeVisible();
  await expect(page.getByText("MCP · MCP fixture", { exact: true })).toBeVisible();
  await expect(page.getByRole("button", { name: "移除 MCP MCP fixture" })).toBeHidden();
  await seeded_session.click();
  await expect(session_header.getByRole("button", { name: "M2 临时会话" })).toBeVisible();
  await expectComposerAtBottom(page);

  const composer = page.getByRole("textbox", { name: "输入消息" });
  await expect(composer).toBeVisible();
  await expect(page.getByRole("button", { name: "添加附件" })).toBeVisible();
  await expect(page.getByRole("button", { name: "执行设置" })).toContainText("构建 · 询问");
  await expect(page.getByRole("button", { name: "模型设置" })).toBeVisible();
  await expect(page.getByRole("img", { name: /上下文用量/ })).toBeVisible();

  await page.getByRole("button", { name: "执行设置" }).click();
  await expect(page.getByRole("menuitem", { name: /执行模式.*构建/ })).toBeVisible();
  await expect(page.getByRole("menuitem", { name: /审批模式.*询问/ })).toBeVisible();
  await page.getByRole("menuitem", { name: /执行模式.*构建/ }).click();
  await expect(page.getByRole("menuitemradio", { name: /规划/ })).toBeVisible();
  await page.keyboard.press("Escape");
  await expect(page.getByRole("menuitemradio", { name: /规划/ })).toBeHidden();
  await composer.click();
  await expect(page.getByRole("menuitem", { name: /执行模式.*构建/ })).toBeHidden();

  await composer.fill("/goal");
  await composer.press("Enter");
  const cancel_goal_tag = page.getByRole("button", { name: "取消目标标记" });
  await expect(cancel_goal_tag).toBeVisible();
  await composer.press("Escape");
  await expect(cancel_goal_tag).toBeVisible();
  await cancel_goal_tag.click();
  await expect(cancel_goal_tag).toBeHidden();

  await composer.fill("第一行");
  await composer.press("Shift+Enter");
  await composer.type("第二行");
  await expect(composer).toHaveValue("第一行\n第二行");
  await composer.fill("FIRST_CASE");
  await composer.fill("/mcp");
  await page.getByRole("button", { name: "发送消息" }).click();
  const mcp_search = page.getByRole("combobox", { name: "搜索MCP 服务" });
  await expect(mcp_search).toBeFocused();
  await mcp_search.fill("local_fixture");
  await expect(page.getByRole("option", { name: /MCP fixture \(local_fixture\)/ })).toBeVisible();
  await mcp_search.press("Enter");
  await composer.fill("FIRST_CASE");
  await composer.press("Enter");
  await expect(composer).toHaveValue("");
  await expect(page.getByText("FIRST_CASE", { exact: true })).toBeVisible();
  await expect(page.getByText("离线回复：FIRST_CASE", { exact: true })).toBeVisible();
  await expect(page.getByText("MCP · MCP fixture", { exact: true })).toBeVisible();
  await expect(page.getByRole("button", { name: "模型设置" })).toBeEnabled();

  await navigation.getByRole("button", { name: "搜索会话" }).click();
  const title_search = page.getByRole("dialog", { name: "搜索会话" }).getByRole("searchbox", { name: "搜索会话名称" });
  await title_search.fill("SOURCE_CASE");
  await page.getByRole("dialog", { name: "搜索会话" }).getByRole("button", { name: /^SOURCE_CASE/ }).click();
  await expect(session_header.getByRole("button", { name: "SOURCE_CASE" })).toBeVisible();
  await navigation.getByRole("button", { name: "返回上一处会话位置" }).click();
  await expect(session_header.getByRole("button", { name: "M2 临时会话" })).toBeVisible();

  await composer.fill("TOOL_CASE");
  await composer.press("Enter");
  await expect(page.getByText("工具执行完成。", { exact: true })).toBeVisible();
  await expect(session_header.getByText("空闲", { exact: true })).toHaveCount(0);

  const context_panel = page.getByRole("tabpanel", { name: "当前上下文" });
  await expect(context_panel.getByText("图片理解").locator("xpath=following-sibling::*[1]")).toContainText("当前不可用");
  const workspace_section = context_panel.getByRole("button", { name: "工作区", exact: true });
  const open_workspace_directory = context_panel.getByRole("button", { name: /^打开 / }).first();
  await expect(open_workspace_directory).toBeVisible();
  await workspace_section.click();
  await expect(open_workspace_directory).toBeHidden();
  await workspace_section.click();
  await expect(open_workspace_directory).toBeVisible();

  const attachment_section = context_panel.getByRole("button", { name: "会话附件 · 1", exact: true });
  await expect(attachment_section).toBeVisible();
  await expect(context_panel.getByRole("button", { name: /e2e-attachment\.txt/ })).toBeVisible();
  await expect(context_panel.getByRole("button", { name: /e2e-context-file\.txt/ })).toHaveCount(0);

  await page.getByRole("button", { name: /write file 完成 e2e-context-file\.txt/ }).click();
  await expect(page.getByRole("dialog", { name: "write_file" })).toBeVisible();
  await page.getByRole("button", { name: "关闭工具详情" }).click();

  const around_run_request = page.waitForRequest((request) => {
    if (!request.url().endsWith("/commands") || request.method() !== "POST") {
      return false;
    }
    return request.postDataJSON()?.command?.payload?.type === "get_conversation_page_around_run";
  });
  await context_panel.getByRole("button", { name: /运行 #1.*0 个工具.*完成/ }).first().click();
  await around_run_request;
  await expect(page.getByText("FIRST_CASE", { exact: true })).toBeVisible();

  await composer.fill("DELEGATE_CASE");
  await composer.press("Enter");
  await expect(page.getByText("允许执行 delegate_task？", { exact: true })).toBeVisible();
  await page.getByRole("radio", { name: /仅本次/ }).check();
  await page.getByRole("button", { name: "允许执行" }).click();
  const child_task = context_panel.getByRole("button", { name: /E2E 子任务/ });
  await expect(child_task).toBeVisible();
  await child_task.click();
  const child_header = page.getByRole("region", { name: "子任务标题栏" });
  await expect(child_header.getByText("E2E 子任务", { exact: true })).toBeVisible();
  await expect(page.getByText("离线回复：DEFAULT_CASE", { exact: true })).toBeVisible();
  await child_header.getByRole("button", { name: "返回主会话" }).click();
  await page.reload();
  await expect(page.locator("[data-runtime-entry]")).toBeVisible();
  await page.getByRole("button", { name: "进入项目" }).click();
  await expect(navigation.getByText("Runtime 已连接")).toBeVisible();
  await expect(session_header.getByRole("button", { name: "M2 临时会话" })).toBeVisible();
  await expect(context_panel.getByRole("button", { name: /E2E 子任务/ })).toBeVisible();
  await expect(page.getByText("离线回复：DELEGATE_CASE", { exact: true })).toBeVisible();
  await seeded_session.click();
  await expect(page.getByText("离线回复：FIRST_CASE", { exact: true })).toBeVisible();

  const renamed_title = "M2 临时会话已重命名";
  await session_header.getByRole("button", { name: "M2 临时会话" }).click();
  const title_input = session_header.getByRole("textbox", { name: "会话标题" });
  await title_input.fill(renamed_title);
  await title_input.press("Enter");
  await expect(session_header.getByRole("button", { name: renamed_title })).toBeVisible();
  await expect(navigation.getByRole("button", { name: renamed_title, exact: true })).toBeVisible();

  await page.reload();
  await expect(page.locator("[data-runtime-entry]")).toBeVisible();
  await page.getByRole("button", { name: "进入项目" }).click();
  await expect(navigation.getByText("Runtime 已连接")).toBeVisible();
  await navigation.getByRole("button", { name: renamed_title, exact: true }).click();
  await expect(session_header.getByRole("button", { name: renamed_title })).toBeVisible();

  await page.getByRole("button", { name: "从此消息分叉" }).last().click();
  await expect(page.getByRole("dialog", { name: "从这条回复创建分支？" })).toBeVisible();
  await page.getByRole("button", { name: "创建分支" }).click();
  const forked_title = `${renamed_title}（分支）`;
  await expect(session_header.getByRole("button", { name: forked_title })).toBeVisible();
  await expect(page.getByText("离线回复：FIRST_CASE", { exact: true })).toBeVisible();
  await expect(page.getByRole("status").filter({ hasText: "继承 · 已完成" }).first()).toBeVisible();

  await session_header.getByRole("button", { name: "更多会话操作" }).click();
  await page.getByRole("menuitem", { name: "永久删除" }).click();
  const delete_dialog = page.getByRole("dialog", { name: "永久删除这个会话？" });
  await expect(delete_dialog).toContainText(/将删除 \d+ 条消息、\d+ 条运行记录/);
  await expect(delete_dialog).toContainText("工作目录中的用户文件不会被删除");
  await delete_dialog.getByRole("button", { name: "永久删除" }).click();
  await expect(session_header.getByRole("button", { name: forked_title })).toBeHidden();
  await expect(navigation.getByRole("button", { name: new RegExp(forked_title) })).toHaveCount(0);

  const added_workspace_name = basename(new_workspace);
  const workspace_backup = backupIsolatedRuntimeBeforeWorkspaceRemoval();
  await navigation.getByRole("button", { name: `${added_workspace_name} 项目操作` }).click();
  await page.getByRole("menuitem", { name: "移除项目…", exact: true }).click();
  const remove_workspace_dialog = page.getByRole("dialog", {
    name: `移除项目“${added_workspace_name}”？`,
  });
  await expect(remove_workspace_dialog).toBeVisible();
  await remove_workspace_dialog.getByRole("button", { name: "移除项目" }).click();
  await expect(navigation.getByRole("button", { name: `${added_workspace_name} 项目操作` })).toHaveCount(0);
  expect(workspaceLifecycle(workspace_backup.source, workspace_backup.workspace_id)).toBe("removed");
  expect(databaseCount(workspace_backup.source, "workspaces")).toBe(workspace_backup.workspace_count);
  expect(databaseCount(workspace_backup.source, "sessions")).toBe(workspace_backup.session_count);

  await navigation.getByRole("button", { name: "恢复已移除项目" }).click();
  const restore_dialog = page.getByRole("dialog", { name: "恢复已移除项目" });
  await expect(restore_dialog.getByText(added_workspace_name, { exact: true })).toBeVisible();
  await restore_dialog.getByRole("button", { name: "恢复", exact: true }).click();
  await expect(restore_dialog).toBeHidden();
  expect(workspaceLifecycle(workspace_backup.source, workspace_backup.workspace_id)).toBe("active");
  expect(databaseCount(workspace_backup.source, "workspaces")).toBe(workspace_backup.workspace_count);
  expect(databaseCount(workspace_backup.source, "sessions")).toBe(workspace_backup.session_count);
  await expect(navigation.getByRole("button", { name: `${added_workspace_name} 项目操作` })).toBeVisible();
  await navigation.getByRole("button", { name: `${added_workspace_name} 项目操作` }).click();
  await expect(page.getByRole("menuitem", { name: "在此新建会话" })).toBeVisible();
  await page.keyboard.press("Escape");
  await expectMcpAcceptance(page);

  await navigation.getByRole("button", { name: "添加项目" }).click();
  await expect(create_workspace_dialog.getByText(/仅使用私有目录/)).toBeVisible();
  await create_workspace_dialog.getByRole("textbox", { name: /项目名称/ }).fill("私有 E2E 工作空间");
  await create_workspace_dialog.getByRole("button", { name: "创建" }).click();
  await expect(create_workspace_dialog).toBeHidden();
  await expectDraftComposerLayout(page);
  await expect(page.getByRole("button", { name: "新会话项目" })).toContainText("私有 E2E 工作空间");
  await page.getByRole("button", { name: "整理文档" }).click();
  const private_composer = page.getByRole("textbox", { name: "输入消息" });
  await expect(private_composer).toHaveValue(/请整理我提供的材料/);
  await expect(session_header.getByRole("button", { name: "新对话" })).toHaveCount(0);
  await private_composer.fill("PRIVATE_CASE");
  await private_composer.press("Enter");
  await expect(page.getByText("离线回复：PRIVATE_CASE", { exact: true })).toBeVisible();
});

async function expectSettingsScrollWithoutFlicker(page: Page): Promise<void> {
  await openSettings(page);
  const settings = page.getByRole("dialog", { name: "设置" });
  await expect(settings.getByRole("region", { name: "当前连接" })).toContainText("这台电脑");
  await settings.screenshot({ path: test.info().outputPath("runtime-overview.png") });
  await settings.getByRole("button", { name: "切换", exact: true }).click();
  await expect(settings.getByRole("heading", { name: "切换 Runtime" })).toBeVisible();
  await settings.getByRole("button", { name: "返回 Runtime" }).click();
  await settings.getByRole("button", { name: /^本机与客户端/ }).click();
  await expect(settings.getByRole("button", { name: "停止本机 Runtime" })).toBeVisible();
  await settings.getByRole("button", { name: "返回 Runtime" }).hover();
  await settings.screenshot({ path: test.info().outputPath("runtime-local.png") });
  await settings.getByRole("button", { name: "返回 Runtime" }).click();
  await settings.getByRole("button", { name: /^状态与诊断/ }).click();
  await expect(settings.getByRole("button", { name: "复制诊断" })).toBeVisible();
  await expect(settings.getByRole("button", { name: "停止本机 Runtime" })).toHaveCount(0);
  await settings.getByRole("button", { name: "返回 Runtime" }).click();
  await settings.getByRole("button", { name: /^访问设置/ }).click();
  const heading = settings.getByRole("heading", { name: "访问设置" });
  await expect(heading).toBeVisible();
  await expect(settings.getByRole("button", { name: "保存访问设置" })).toBeVisible();
  await settings.screenshot({ path: test.info().outputPath("runtime-access.png") });
  const result = await settings.getByRole("button", { name: "保存访问设置" }).evaluate(async (element) => {
    let content = element.parentElement;
    while (content && getComputedStyle(content).overflowY !== "auto") content = content.parentElement;
    const overlays = document.getElementById("overlay-root");
    if (!content || !overlays) throw new Error("Settings scroll container or portal is missing");
    let hidden = false;
    const observer = new MutationObserver((records) => {
      hidden ||= getComputedStyle(overlays).visibility === "hidden"
        || records.some((record) => record.oldValue?.includes("visibility: hidden"));
    });
    observer.observe(overlays, { attributes: true, attributeFilter: ["style"], attributeOldValue: true });
    let max_scroll = 0;
    try {
      for (let index = 0; index < 10; index += 1) {
        content.scrollTop = index % 2 === 0 ? content.scrollHeight : 0;
        max_scroll = Math.max(max_scroll, content.scrollTop);
        await new Promise<void>((resolve) => setTimeout(resolve, 60));
      }
    } finally { observer.disconnect(); }
    return { hidden, max_scroll };
  });
  expect(result.max_scroll).toBeGreaterThan(0);
  expect(result.hidden).toBe(false);
  await settings.getByRole("button", { name: "关闭设置" }).click();
}

async function expectProviderConnectionForm(page: Page): Promise<void> {
  await openSettings(page);
  const settings = page.getByRole("dialog", { name: "设置" });
  await settings.getByRole("button", { name: "模型", exact: true }).click();
  await settings.getByRole("button", { name: "添加服务商" }).click();
  const provider = settings.getByRole("button", { name: "服务商类型" });
  await provider.click();
  const provider_options = page.getByRole("listbox", { name: "服务商类型" });
  await expect(provider_options.getByRole("option")).toHaveCount(8);
  await expect(provider_options.getByRole("option", { name: "Kimi / Moonshot" })).toBeVisible();
  const trigger_width = await provider.evaluate((element) => element.getBoundingClientRect().width);
  const popup_width = await provider_options.evaluate((element) => element.getBoundingClientRect().width);
  expect(popup_width).toBeGreaterThanOrEqual(trigger_width);
  await provider_options.getByRole("option", { name: "Kimi / Moonshot" }).click();
  await expect(provider).toContainText("Kimi / Moonshot");
  await expect(settings.getByLabel("API Key", { exact: true })).toHaveValue("");

  await settings.getByRole("button", { name: "取消" }).click();
  const discard_dialog = page.getByRole("dialog", { name: "放弃未保存的修改？" });
  await expect(discard_dialog).toBeVisible();
  await discard_dialog.getByRole("button", { name: "放弃修改" }).click();
  await expect(discard_dialog).toBeHidden();
  await settings.getByRole("button", { name: "关闭设置" }).click();
}

async function expectDeviceGatewayManagement(page: Page): Promise<void> {
  await openSettings(page);
  const settings = page.getByRole("dialog", { name: "设置" });
  await settings.getByRole("button", { name: "智能终端", exact: true }).click();
  await expect(settings.getByText("语音识别").locator("xpath=following-sibling::*[1]"))
    .toHaveText("不可用");
  await expect(settings.getByText("语音播放").locator("xpath=following-sibling::*[1]"))
    .toHaveText("不可用");

  const access = settings.getByRole("switch", { name: "智能终端接入" });
  await expect(access).toHaveAttribute("aria-checked", "false");
  await access.click();
  await expect(access).toHaveAttribute("aria-checked", "true");

  await settings.getByRole("button", { name: "添加设备", exact: true }).click();
  await expect(settings.getByText("正在等待终端发起配对…", { exact: true })).toBeVisible();
  await settings.getByRole("button", { name: "结束添加", exact: true }).click();
  await expect(settings.getByText("点击“添加设备”后，附近未配对终端才能申请接入。", { exact: true }))
    .toBeVisible();

  await access.click();
  await expect(access).toHaveAttribute("aria-checked", "false");
  await settings.getByRole("button", { name: "关闭设置" }).click();
}

async function expectComposerAtBottom(page: Page): Promise<void> {
  const composer = page.getByRole("textbox", { name: "输入消息" });
  await expect(composer).toBeVisible();
  const box = await composer.boundingBox();
  const viewport = page.viewportSize();
  expect(box).not.toBeNull();
  expect(viewport).not.toBeNull();
  expect(box!.y).toBeGreaterThan(viewport!.height - 150);
}

async function expectDraftComposerLayout(page: Page): Promise<void> {
  const drawer = page.getByRole("region", { name: "新会话项目选项" });
  const composer = page.getByRole("textbox", { name: "输入消息" });
  await expect(drawer).toBeVisible();
  await expect(page.getByRole("button", { name: "整理文档" })).toBeVisible();
  const drawer_box = await drawer.boundingBox();
  const composer_box = await composer.boundingBox();
  expect(drawer_box && composer_box && drawer_box.y < composer_box.y).toBe(true);
  const trigger_box = await drawer.getByRole("button", { name: "新会话项目" }).boundingBox();
  const composer_section_box = await drawer.locator("xpath=following-sibling::section[1]").boundingBox();
  if (!drawer_box || !trigger_box || !composer_section_box) throw new Error("Draft drawer geometry is unavailable");
  const top_gap = trigger_box.y - drawer_box.y;
  const bottom_gap = composer_section_box.y - trigger_box.y - trigger_box.height;
  expect(Math.abs(top_gap - bottom_gap)).toBeLessThanOrEqual(2);
  const dock_box = await drawer.locator("xpath=..").boundingBox();
  const viewport = page.viewportSize();
  expect(dock_box && viewport && dock_box.y + dock_box.height > viewport.height - 40).toBe(true);
}

function backupIsolatedRuntimeBeforeWorkspaceRemoval(): Readonly<{
  source: string;
  workspace_id: string;
  workspace_count: number;
  session_count: number;
}> {
  const runtime_home = process.env.EZ_ASSISTANT_E2E_RUNTIME_HOME;
  if (!runtime_home || !runtime_home.includes("ez-assistant-e2e-runtime-")) throw new Error("Not an isolated E2E Runtime Home");
  const source = join(runtime_home, "users", "_personal", "data", "runtime.sqlite3");
  const backup_root = resolve("../../.runtime-test/c02-m4");
  const backup_directory = mkdtempSync(join(backup_root, "pre-remove-backup-"));
  const backup = join(backup_directory, "runtime.sqlite3");
  execFileSync("sqlite3", [source, `.backup '${backup}'`]);
  if (statSync(backup).size === 0) throw new Error("Workspace backup is empty");
  for (const table of ["workspaces", "sessions"]) {
    const source_count = databaseCount(source, table);
    const backup_count = databaseCount(backup, table);
    if (source_count !== backup_count) throw new Error(`${table} backup count differs: ${source_count} vs ${backup_count}`);
  }
  const source_rows = execFileSync("sqlite3", [source, "SELECT workspace_id || ':' || lifecycle FROM workspaces ORDER BY workspace_id"], { encoding: "utf8" });
  const backup_rows = execFileSync("sqlite3", [backup, "SELECT workspace_id || ':' || lifecycle FROM workspaces ORDER BY workspace_id"], { encoding: "utf8" });
  if (source_rows !== backup_rows) throw new Error("Workspace backup identities differ");
  const workspace_id = execFileSync("sqlite3", [source, "SELECT workspace_id FROM workspaces WHERE label LIKE 'ez-assistant-e2e-added-workspace-%' AND lifecycle = 'active'"], { encoding: "utf8" }).trim();
  if (!/^[A-Za-z0-9_-]+$/.test(workspace_id)) throw new Error("Could not resolve isolated workspace ID");
  console.log(`Verified isolated pre-removal backup: ${backup}`);
  return { source, workspace_id, workspace_count: databaseCount(source, "workspaces"), session_count: databaseCount(source, "sessions") };
}

function databaseCount(database: string, table: "workspaces" | "sessions"): number {
  const count = Number(execFileSync("sqlite3", [database, `SELECT COUNT(*) FROM ${table}`], { encoding: "utf8" }).trim());
  if (!Number.isSafeInteger(count)) throw new Error(`${table} count is invalid`);
  return count;
}

function workspaceLifecycle(database: string, workspace_id: string): string {
  return execFileSync("sqlite3", [database, `SELECT lifecycle FROM workspaces WHERE workspace_id = '${workspace_id}'`], { encoding: "utf8" }).trim();
}

async function selectMcpFixture(page: Page): Promise<void> {
  const composer = page.getByRole("textbox", { name: "输入消息" });
  await composer.fill("/mcp");
  await composer.press("Enter");
  await page.getByRole("option", { name: /MCP fixture \(local_fixture\)/ }).click();
  await expect(page.getByRole("button", { name: "移除 MCP MCP fixture" })).toBeVisible();
  await expect(composer).toBeFocused();
}

async function expectResponsiveLayouts(page: Page): Promise<void> {
  const layouts = [
    { width: 1440, height: 900, left_sidebar: true, right_sidebar: true },
    // 右栏可缩到 320；1152 仍可同时保留左右栏。
    { width: 1152, height: 720, left_sidebar: true, right_sidebar: true },
    { width: 960, height: 640, left_sidebar: true, right_sidebar: false },
    // 左栏沿用用户展开偏好，窄窗口只自动收起右栏。
    { width: 720, height: 720, left_sidebar: true, right_sidebar: false },
  ] as const;

  for (const layout of layouts) {
    await page.setViewportSize({ width: layout.width, height: layout.height });
    const left_sidebar = page.getByRole("complementary", { name: "会话导航" });
    const right_sidebar = page.getByRole("complementary", { name: "资源栏" });
    if (layout.left_sidebar) {
      await expect(left_sidebar).toBeVisible();
      await expect(left_sidebar.getByRole("button", { name: "收起会话栏" })).toBeVisible();
      await expect(left_sidebar.getByRole("button", { name: "收起会话栏" }).locator("svg"))
        .toHaveAttribute("data-icon", "sidebar-left");
      const left_gap = await left_sidebar.locator('[data-tauri-drag-region="deep"]').evaluate((region) => {
        const navigation = region.querySelector<HTMLElement>('nav[aria-label="会话导航操作"]');
        return navigation ? navigation.getBoundingClientRect().left - region.getBoundingClientRect().left : -1;
      });
      expect(left_gap).toBe(12);
    } else {
      await expect(left_sidebar).toBeHidden();
      await expect(page.locator('header[aria-label="会话标题栏"]').getByRole("button", { name: "展开会话栏" })).toBeVisible();
      await expect(page.locator('header[aria-label="会话标题栏"]').getByRole("button", { name: "展开会话栏" }).locator("svg"))
        .toHaveAttribute("data-icon", "sidebar-left");
    }
    await expect(right_sidebar).toBeVisible({ visible: layout.right_sidebar });
    if (layout.left_sidebar) await expect(page.getByRole("button", { name: "账号与 Runtime" })).toBeVisible();
    await expect(page.locator('header[aria-label="会话标题栏"]')).toHaveAttribute("data-tauri-drag-region", "deep");
    await expect(page.locator('aside[aria-label="资源栏"] header')).toHaveAttribute("data-tauri-drag-region", "deep");
    const nav_buttons = page.getByRole("navigation", { name: "会话导航操作" }).getByRole("button");
    await expect(page.getByRole("navigation", { name: "会话导航操作" }).locator("img")).toBeVisible();
    expect(await nav_buttons.evaluateAll((buttons) => buttons.map((button) => ({
      height: button.getBoundingClientRect().height,
      background: getComputedStyle(button).backgroundColor,
    })))).toEqual(Array.from({ length: 5 }, () => ({ height: 24, background: "rgba(0, 0, 0, 0)" })));
    if (layout.width === 1440) {
      await expect(left_sidebar.getByRole("region", { name: "置顶会话" })).toHaveCount(0);
      const sidebar_rows = await left_sidebar.evaluate((sidebar) => {
        const quick = sidebar.querySelector<HTMLButtonElement>('button[aria-label="新建会话"]');
        const folder = sidebar.querySelector<SVGElement>('button[aria-expanded] svg[data-icon="folder"]');
        const workspace = folder?.closest<HTMLButtonElement>("button");
        const heading = Array.from(sidebar.querySelectorAll<HTMLButtonElement>("button[aria-expanded]"))
          .find((button) => button.textContent?.trim() === "项目");
        if (!quick || !folder || !workspace || !heading) throw new Error("Sidebar rows not found");
        return {
          quick_height: quick.getBoundingClientRect().height,
          icon_offset: quick.querySelector("svg")!.getBoundingClientRect().left - folder.getBoundingClientRect().left,
          label_offset: quick.querySelector("span")!.getBoundingClientRect().left - workspace.querySelector("span")!.getBoundingClientRect().left,
          heading_font_size: getComputedStyle(heading).fontSize,
        };
      });
      expect(sidebar_rows.quick_height).toBe(32);
      expect(Math.abs(sidebar_rows.icon_offset)).toBeLessThanOrEqual(1);
      expect(Math.abs(sidebar_rows.label_offset)).toBeLessThanOrEqual(1);
      expect(sidebar_rows.heading_font_size).toBe("14px");
      await page.screenshot({ path: test.info().outputPath("sidebar-navigation-alignment.png"), animations: "disabled" });
    }

    const top_geometry = await page.locator("main").evaluate((element) => {
      const conversation = element.querySelector<HTMLElement>("[data-conversation-area]");
      const header = element.querySelector<HTMLElement>('header[aria-label="会话标题栏"]');
      const left = element.querySelector<HTMLElement>('[data-shell-sidebar="left"]');
      return {
        conversation_x: conversation?.getBoundingClientRect().x,
        header_height: header?.getBoundingClientRect().height,
        left_width: left?.getBoundingClientRect().width,
      };
    });
    expect(top_geometry.header_height).toBe(42);
    if (!layout.left_sidebar) {
      expect(top_geometry.left_width).toBe(0);
      expect(top_geometry.conversation_x).toBe(0);
    }

    const geometry = await page.locator("main").evaluate((element) => ({
      client_width: element.clientWidth,
      scroll_width: element.scrollWidth,
      height: element.getBoundingClientRect().height,
    }));
    expect(geometry.scroll_width).toBeLessThanOrEqual(geometry.client_width);
    expect(geometry.height).toBeLessThanOrEqual(layout.height);
  }

  await page.setViewportSize({ width: 1440, height: 900 });
  const header = page.locator('header[aria-label="会话标题栏"]');
  await header.getByRole("button", { name: "收起资源栏" }).click();
  await expect(page.getByRole("complementary", { name: "资源栏" })).toBeHidden();
  await header.getByRole("button", { name: "展开资源栏" }).click();
  await expect(page.getByRole("complementary", { name: "资源栏" })).toBeVisible();

  await page.getByRole("complementary", { name: "会话导航" }).getByRole("button", { name: "收起会话栏" }).click();
  await expect(page.getByRole("complementary", { name: "会话导航" })).toBeHidden();
  const macos_navigation_offset = await header.evaluate((element) => {
    const shell = element.closest<HTMLElement>("main");
    if (!shell) throw new Error("App shell not found");
    const original_platform = shell.dataset.platform;
    shell.dataset.platform = "macos";
    const navigation = element.querySelector<HTMLElement>('nav[aria-label="会话导航操作"]');
    const offset = navigation ? navigation.getBoundingClientRect().left - element.getBoundingClientRect().left : -1;
    if (original_platform === undefined) delete shell.dataset.platform;
    else shell.dataset.platform = original_platform;
    return offset;
  });
  expect(macos_navigation_offset).toBe(96);
  await header.getByRole("button", { name: "搜索会话" }).click();
  await expect(page.getByRole("dialog", { name: "搜索会话" }).getByRole("searchbox", { name: "搜索会话名称" })).toBeFocused();
  await expect(page.getByRole("complementary", { name: "会话导航" })).toBeHidden();
  await page.getByRole("dialog", { name: "搜索会话" }).getByRole("button", { name: "关闭搜索" }).click();
  await header.getByRole("button", { name: "展开会话栏" }).click();
}

async function openSettings(page: Page): Promise<void> {
  await page.getByRole("button", { name: "账号与 Runtime" }).click();
  await page.getByRole("menuitem", { name: "设置" }).click();
}
