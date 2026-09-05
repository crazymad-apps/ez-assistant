import { expect, test } from "@playwright/test";
import type { Page } from "@playwright/test";
import { basename } from "node:path";
import { expectMcpAcceptance } from "./mcp-acceptance";

test("loads real workspaces and sessions from the temporary Runtime Host", async ({ page }) => {
  test.setTimeout(60_000);
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
    headers: { Authorization: `Bearer ${runtime_bootstrap.access_token}` },
  });
  expect(retired_demo.status).toBe(404);
  await page.addInitScript(({ serialized_bootstrap, workspace_directory }) => {
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
              expanded_workspace_ids: null,
            });
          }
          if (command === "save_desktop_preferences") {
            return Promise.resolve();
          }
          if (command === "choose_workspace_directory") {
            return Promise.resolve(workspace_directory);
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
              headers: { Authorization: `Bearer ${runtime.access_token}` },
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
  }, { serialized_bootstrap: bootstrap, workspace_directory: new_workspace });

  await page.goto("/");
  await expect(page).toHaveTitle("EZ Assistant");
  await expect(page.getByText("ez-assistant · 本地 AI 助手")).toBeVisible();
  await expect(page.getByRole("button", { name: "新对话" })).toBeVisible();
  await expect(page.getByRole("tabpanel", { name: "当前上下文" })).toBeVisible();
  await expect(page.getByText("运行时已连接")).toBeVisible();
  await expectResponsiveLayouts(page);
  await expectModelCatalogForm(page);
  await expectDeviceGatewayManagement(page);
  const navigation = page.getByRole("complementary", { name: "会话导航" });
  const session_header = page.locator('header[aria-label="会话标题栏"]');
  const seeded_session = navigation.getByRole("button", { name: /^M2 临时会话(?:\s|$)/ });
  await expect(seeded_session).toBeVisible();
  await expect(session_header.getByRole("button", { name: "M2 临时会话" })).toBeVisible();

  await navigation.getByRole("button", { name: "添加工作空间" }).click();
  const create_workspace_dialog = page.getByRole("dialog", { name: "新建工作空间" });
  await expect(create_workspace_dialog.getByRole("textbox", { name: /工作空间名称/ }))
    .toHaveValue(basename(new_workspace));
  await create_workspace_dialog.getByRole("button", { name: "保存" }).click();
  await expect(create_workspace_dialog).toBeHidden();
  await expect(navigation.getByText(basename(new_workspace), { exact: true })).toBeVisible();
  await expect(session_header.getByRole("button", { name: "新对话" })).toBeVisible();
  await expectComposerAtBottom(page);
  await seeded_session.click();
  await expect(session_header.getByRole("button", { name: "M2 临时会话" })).toBeVisible();

  await navigation.getByRole("button", { name: "搜索会话" }).click();
  const search_input = navigation.getByRole("searchbox", { name: "搜索会话名称" });
  await search_input.fill("M2 临时");
  await expect(navigation.getByRole("button", { name: /M2 临时会话/ })).toBeVisible();
  await navigation.getByRole("button", { name: "关闭搜索" }).click();

  const workspace_menu_trigger = navigation.getByRole("button", { name: /工作空间操作/ }).first();
  await workspace_menu_trigger.click();
  await expect(page.getByRole("menu", { name: /工作空间操作/ })).toBeVisible();
  await expect(page.getByRole("menuitem", { name: "在此新建会话" })).toBeVisible();
  await expect(page.getByRole("menuitem", { name: "打开工作目录" })).toBeVisible();
  await expect(page.getByRole("menuitem", { name: "复制目录路径" })).toBeVisible();
  await expect(page.getByRole("menuitem", { name: "移除工作空间…" })).toBeVisible();
  await page.keyboard.press("Escape");
  await expect(page.getByRole("menu", { name: /工作空间操作/ })).toBeHidden();

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

  await navigation.getByRole("button", { name: "新对话", exact: true }).click();
  await page.getByRole("menu", { name: "选择新会话目录" }).getByRole("menuitem").first().click();
  await expect(session_header.getByRole("button", { name: "新对话" })).toBeVisible();
  await expectComposerAtBottom(page);
  const source_composer = page.getByRole("textbox", { name: "输入消息" });
  await selectMcpFixture(page);
  await source_composer.fill("SOURCE_CASE");
  await source_composer.press("Enter");
  await expect(source_composer).toHaveValue("");
  await expect(page.getByTitle("SOURCE_CASE")).toBeVisible();
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
  await page.keyboard.press("Escape");
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
  const title_search = navigation.getByRole("searchbox", { name: "搜索会话名称" });
  await title_search.fill("SOURCE_CASE");
  await navigation.getByRole("button", { name: /^SOURCE_CASE/ }).click();
  await expect(session_header.getByRole("button", { name: "SOURCE_CASE" })).toBeVisible();
  await session_header.getByRole("button", { name: "返回上一处会话位置" }).click();
  await expect(session_header.getByRole("button", { name: "M2 临时会话" })).toBeVisible();
  await navigation.getByRole("button", { name: "关闭搜索" }).click();

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
  await expect(page.getByText("运行时已连接")).toBeVisible();
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
  await expect(navigation.getByRole("button", { name: new RegExp(renamed_title) })).toBeVisible();

  await page.reload();
  await expect(page.getByText("运行时已连接")).toBeVisible();
  await navigation.getByRole("button", { name: new RegExp(renamed_title) }).click();
  await expect(session_header.getByRole("button", { name: renamed_title })).toBeVisible();

  await page.getByRole("button", { name: "从此消息分叉" }).last().click();
  await expect(page.getByRole("dialog", { name: "从这条回复创建分支？" })).toBeVisible();
  await page.getByRole("button", { name: "创建分支" }).click();
  const forked_title = `${renamed_title}（分支）`;
  await expect(session_header.getByRole("button", { name: forked_title })).toBeVisible();
  await expect(page.getByText("离线回复：FIRST_CASE", { exact: true })).toBeVisible();

  await session_header.getByRole("button", { name: "更多会话操作" }).click();
  await page.getByRole("menuitem", { name: "永久删除" }).click();
  const delete_dialog = page.getByRole("dialog", { name: "永久删除这个会话？" });
  await expect(delete_dialog).toContainText(/将删除 \d+ 条消息、\d+ 条运行记录/);
  await expect(delete_dialog).toContainText("工作目录中的用户文件不会被删除");
  await delete_dialog.getByRole("button", { name: "永久删除" }).click();
  await expect(session_header.getByRole("button", { name: forked_title })).toBeHidden();
  await expect(navigation.getByRole("button", { name: new RegExp(forked_title) })).toHaveCount(0);

  const added_workspace_name = basename(new_workspace);
  await navigation.getByRole("button", { name: `${added_workspace_name} 工作空间操作` }).click();
  await page.getByRole("menuitem", { name: "移除工作空间…" }).click();
  const remove_workspace_dialog = page.getByRole("dialog", {
    name: `移除工作空间“${added_workspace_name}”？`,
  });
  await expect(remove_workspace_dialog).toBeVisible();
  await remove_workspace_dialog.getByRole("button", { name: "移除工作空间" }).click();
  await expect(navigation.getByRole("button", { name: `${added_workspace_name} 工作空间操作` })).toHaveCount(0);
  await navigation.getByRole("button", { name: "新对话", exact: true }).click();
  await expect(
    page.getByRole("menu", { name: "选择新会话目录" }).getByRole("menuitem", { name: added_workspace_name }),
  ).toHaveCount(0);
  await page.keyboard.press("Escape");

  await navigation.getByRole("button", { name: "添加工作空间" }).click();
  await expect(create_workspace_dialog).toBeVisible();
  await create_workspace_dialog.getByRole("button", { name: "保存" }).click();
  await expect(create_workspace_dialog).toBeHidden();
  await expect(navigation.getByRole("button", { name: `${added_workspace_name} 工作空间操作` })).toBeVisible();
  await navigation.getByRole("button", { name: "新对话", exact: true }).click();
  await expect(
    page.getByRole("menu", { name: "选择新会话目录" }).getByRole("menuitem", { name: added_workspace_name }),
  ).toBeVisible();
  await page.keyboard.press("Escape");
  await expectMcpAcceptance(page);
});

async function expectModelCatalogForm(page: Page): Promise<void> {
  await page.getByRole("button", { name: "设置", exact: true }).click();
  const settings = page.getByRole("dialog", { name: "设置" });
  await settings.getByRole("button", { name: "模型", exact: true }).click();
  await settings.getByRole("button", { name: "添加模型" }).click();
  await expect(settings.getByRole("button", { name: "选择接口协议" })).toContainText(
    "Chat Completions（OpenAI Compatible）",
  );

  const provider = settings.getByRole("combobox", { name: "供应商（Provider）" });
  await provider.click();
  const provider_options = page.getByRole("listbox", { name: "供应商（Provider）" });
  await expect(provider_options.getByRole("option")).toHaveCount(5);
  await expect(provider_options.getByRole("option", { name: /Moonshot（Kimi）/ })).toBeVisible();
  const trigger_width = await provider.evaluate((element) => element.parentElement?.getBoundingClientRect().width ?? 0);
  const popup_width = await provider_options.evaluate((element) => element.getBoundingClientRect().width);
  expect(popup_width).toBeGreaterThanOrEqual(trigger_width);

  await provider_options.getByRole("option", { name: /Moonshot（Kimi）/ }).click();
  await expect(provider).toHaveValue("moonshot");
  const model = settings.getByRole("combobox", { name: "模型 ID" });
  await model.click();
  await expect(page.getByRole("option", { name: "kimi-k3" })).toBeVisible();

  await settings.getByRole("button", { name: "取消" }).click();
  const discard_dialog = page.getByRole("dialog", { name: "放弃未保存的模型修改？" });
  await expect(discard_dialog).toBeVisible();
  await discard_dialog.getByRole("button", { name: "放弃修改" }).click();
  await expect(discard_dialog).toBeHidden();
  await settings.getByRole("button", { name: "关闭设置" }).click();
}

async function expectDeviceGatewayManagement(page: Page): Promise<void> {
  await page.getByRole("button", { name: "设置", exact: true }).click();
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
    { width: 1152, height: 720, left_sidebar: true, right_sidebar: true },
    { width: 960, height: 640, left_sidebar: true, right_sidebar: false },
    { width: 720, height: 720, left_sidebar: false, right_sidebar: false },
  ] as const;

  for (const layout of layouts) {
    await page.setViewportSize({ width: layout.width, height: layout.height });
    const left_sidebar = page.getByRole("complementary", { name: "会话导航" });
    const right_sidebar = page.getByRole("complementary", { name: "资源栏" });
    await expect(left_sidebar).toBeVisible({ visible: layout.left_sidebar });
    await expect(right_sidebar).toBeVisible({ visible: layout.right_sidebar });
    await expect(page.getByText("ez-assistant · 本地 AI 助手")).toBeVisible();
    await expect(page.getByText("运行时已连接")).toBeVisible();

    const geometry = await page.locator("main").evaluate((element) => ({
      client_width: element.clientWidth,
      scroll_width: element.scrollWidth,
      height: element.getBoundingClientRect().height,
    }));
    expect(geometry.scroll_width).toBeLessThanOrEqual(geometry.client_width);
    expect(geometry.height).toBeLessThanOrEqual(layout.height);
  }

  await page.setViewportSize({ width: 1440, height: 900 });
}
