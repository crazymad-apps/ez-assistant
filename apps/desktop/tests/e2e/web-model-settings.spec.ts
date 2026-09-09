import { expect, test } from "@playwright/test";
import { createServer } from "node:http";

// 真实 Web → Host → Runtime → 模拟服务商；不读取用户库或真实凭据。
test("model settings apply provider rules, field templates and wire effort mappings", async ({ page }) => {
  test.setTimeout(90_000);
  const markdown_reply = [
    "DIRECT_TEMPLATE_OK", "",
    ...Array.from({ length: 5 }, (_, index) => `${26 + index}. 有序列表换行检查：这是一段用于验证多位数编号与正文对齐的较长文本。继续补充内容以触发换行，下一行应对齐正文，序号保持完整可见。`),
    "", "嵌套列表", "", "1. 父级内容", "",
    "   998. 嵌套第一项", "   999. 嵌套第二项", "   1000. 嵌套第三项", "",
    "较大起始序号", "", "123456789. 九位数编号", "", "- 无序列表保持缩进",
  ].join("\n");
  let fail_list = false;
  const paths: string[] = [];
  const calls: Record<string, unknown>[] = [];
  const server = createServer(async (request, response) => {
    paths.push(request.url ?? "");
    if (request.method === "GET") {
      response.setHeader("Content-Type", "application/json");
      if (fail_list) { response.writeHead(503); response.end('{}'); return; }
      if (request.url?.startsWith("/api/v1/models")) {
        response.end(JSON.stringify({ success: true, output: { total: 1, page_no: 1, page_size: 50, models: [{ model: "qwen3.8-max", model_info: { context_window: 900000 }, features: ["function-calling"], capabilities: ["Reasoning"], inference_metadata: { request_modality: ["Text", "Image"] } }] } }));
      } else {
        response.end(JSON.stringify({ object: "list", data: [{ id: "qwen3.8-max", object: "model" }, { id: "gpt-4.1", object: "model" }, ...["deepseek-v4-flash", "deepseek-v4-pro", "deepseek-v4-flash-vision-exp", "qwen3.7-max", "qwen3.7-plus", "qwen3.6-flash", "glm-5.2", "deepseek-v4-flash-0731"].map(id => ({ id, object: "model" }))] }));
      }
      return;
    }
    let body = ""; for await (const chunk of request) body += chunk;
    calls.push(JSON.parse(body) as Record<string, unknown>);
    response.writeHead(200, { "Content-Type": "text/event-stream" });
    if (request.url?.endsWith("/responses")) {
      const model = (calls.at(-1)!).model;
      const item = { id: "direct-message", type: "message", role: "assistant", content: [{ type: "output_text", text: markdown_reply, annotations: [] }] };
      for (const event of [
        { type: "response.created", response: { id: "direct-response", model, status: "in_progress" } },
        { type: "response.output_item.added", output_index: 0, item: { ...item, content: [] } },
        { type: "response.output_text.done", item_id: item.id, output_index: 0, content_index: 0, text: markdown_reply },
        { type: "response.output_item.done", output_index: 0, item },
        { type: "response.completed", response: { id: "direct-response", model, status: "completed", usage: { input_tokens: 10, output_tokens: 3, total_tokens: 13 } } },
      ]) response.write(`data: ${JSON.stringify(event)}\n\n`);
      response.end("data: [DONE]\n\n"); return;
    }
    for (const delta of [{ role: "assistant" }, { reasoning_content: "check" }, { content: "OK" }]) response.write(`data: ${JSON.stringify({ id: "functional-check", model: "qwen3.8-max", choices: [{ index: 0, delta }] })}\n\n`);
    response.end(`data: ${JSON.stringify({ id: "functional-check", model: "qwen3.8-max", choices: [{ index: 0, delta: {}, finish_reason: "stop" }], usage: { prompt_tokens: 10, completion_tokens: 2, total_tokens: 12 } })}\n\ndata: [DONE]\n\n`);
  });
  await new Promise<void>(resolve => server.listen(0, "127.0.0.1", resolve));
  const address = server.address();
  if (!address || typeof address === "string") throw new Error("fixture address missing");
  try {
    const fixture = JSON.parse(process.env.EZ_ASSISTANT_E2E_BOOTSTRAP!) as { base_url: string; access_token: string };
    const issued = await fetch(`${fixture.base_url}/auth/login`, { method: "POST", headers: { Authorization: `Bearer ${fixture.access_token}`, "Content-Type": "application/json" }, body: JSON.stringify({ method: "desktop" }) });
    const { token } = await issued.json() as { token: string };
    await page.emulateMedia({ reducedMotion: "reduce" });
    await page.goto(`${fixture.base_url}/#token=${encodeURIComponent(token)}`);
    await page.getByRole("button", { name: "设置", exact: true }).click();
    const dialog = page.getByRole("dialog", { name: "设置", exact: true });
    await dialog.getByRole("button", { name: "模型", exact: true }).click();
    async function select(label: string | RegExp, option: string) {
      await dialog.getByRole("button", { name: label, exact: typeof label === "string" }).click();
      await page.getByRole("option", { name: option, exact: true }).click();
    }
    async function add(label: string, name: string) {
      await dialog.getByRole("button", { name: "添加服务商", exact: true }).click();
      await select("服务商类型", label);
      await dialog.getByLabel("服务商名称", { exact: true }).fill(name);
      await dialog.getByLabel("服务地址", { exact: true }).fill(`http://127.0.0.1:${address.port}/v1`);
      await dialog.getByLabel("API Key", { exact: true }).fill("artificial-functional-key");
    }
    await add("百炼 API", "原生列表验证");
    await expect(dialog.getByLabel("服务商名称", { exact: true })).toHaveAttribute("placeholder", "请输入");
    const key_input = dialog.getByLabel("API Key", { exact: true });
    const show_key = dialog.getByRole("button", { name: "显示本次输入", exact: true });
    await expect(dialog.getByRole("checkbox", { name: "显示本次输入" })).toHaveCount(0);
    await expect(key_input).toHaveAttribute("type", "password");
    await show_key.click();
    await expect(key_input).toHaveAttribute("type", "text");
    await expect(key_input).toHaveValue("artificial-functional-key");
    const hide_key = dialog.getByRole("button", { name: "隐藏本次输入", exact: true });
    await expect(hide_key).toHaveAttribute("aria-pressed", "true");
    await hide_key.press("Space");
    await expect(key_input).toHaveAttribute("type", "password");
    await expect(key_input).toHaveValue("artificial-functional-key");
    const key_bounds = (await key_input.boundingBox())!;
    const eye_bounds = (await show_key.boundingBox())!;
    expect(eye_bounds.x + eye_bounds.width).toBeLessThanOrEqual(key_bounds.x + key_bounds.width);
    expect(eye_bounds.x).toBeGreaterThan(key_bounds.x + key_bounds.width / 2);
    await expect(key_input).toHaveCSS("padding-right", "36px");
    await dialog.screenshot({ path: test.info().outputPath("provider-key-eye.png") });

    await dialog.getByRole("button", { name: "接口选项", exact: true }).click();
    await expect(dialog.getByLabel("模型列表接口", { exact: true })).toHaveValue("");
    await expect(dialog.getByRole("button", { name: "列表格式", exact: true })).toHaveCount(0);
    await dialog.getByLabel("模型列表接口", { exact: true }).fill("/custom/models");
    await select("服务商类型", "Kimi / Moonshot");
    await expect(dialog.getByLabel("模型列表接口", { exact: true })).toHaveValue("/custom/models");
    await dialog.getByLabel("模型列表接口", { exact: true }).fill("");
    await expect(dialog.getByLabel("服务商名称", { exact: true })).toHaveValue("原生列表验证");
    await select("服务商类型", "百炼 API");
    await dialog.getByLabel("API Key", { exact: true }).fill("sk-sp-artificial-functional-key");
    await dialog.getByRole("button", { name: "保存服务商", exact: true }).click();
    await expect(dialog.getByRole("alert")).toContainText("套餐密钥应选择百炼套餐");
    expect(paths).toHaveLength(0);
    await dialog.getByLabel("API Key", { exact: true }).fill("artificial-functional-key");
    await dialog.getByRole("button", { name: "保存服务商", exact: true }).click();
    const delete_provider = dialog.getByRole("button", { name: "删除服务商", exact: true });
    await expect(delete_provider).toBeEnabled();
    await expect(delete_provider).toHaveAttribute("data-button-variant", "danger");
    const danger_background = await delete_provider.evaluate(element => getComputedStyle(element).backgroundColor);
    await delete_provider.hover();
    await expect(delete_provider).toHaveCSS("background-color", danger_background);
    await expect(delete_provider).toHaveCSS("color", "rgb(255, 255, 255)");
    await expect(delete_provider).toHaveCSS("filter", "brightness(0.94)");
    await dialog.screenshot({ path: test.info().outputPath("provider-danger-hover.png") });
    await page.mouse.move(0, 0);
    // 复现前端热更新、独立 Runtime 仍返回旧详情的实际场景。
    let old_detail = true;
    await page.route("**/commands", async route => {
      const command = route.request().postDataJSON() as { command?: { payload?: { type?: string } } };
      if (!old_detail || command.command?.payload?.type !== "get_model_configuration") {
        await route.continue(); return;
      }
      old_detail = false;
      const response = await route.fetch();
      const body = await response.json() as { result: { payload: { payload: { field_sources?: unknown } } } };
      delete body.result.payload.payload.field_sources;
      await route.fulfill({ response, json: body });
    });
    await dialog.getByRole("button", { name: "配置模型 qwen3.8-max", exact: true }).click();
    await expect(dialog.getByRole("alert")).toContainText("请重启对应 Runtime 后重试");
    await expect(page.getByText("桌面界面无法继续显示", { exact: true })).toHaveCount(0);
    await dialog.getByRole("button", { name: "重试读取", exact: true }).click();
    await expect(dialog.getByLabel("上下文窗口（Token）", { exact: true })).toHaveValue("900000");
    await expect(dialog.getByLabel("运行输出上限（Token）", { exact: true })).toHaveValue("131072");
    await expect(dialog.locator("form label small, form span small")).toHaveCount(0);
    await expect(dialog.getByText(/^来源：/)).toHaveCount(0);
    expect(paths.some(path => path.startsWith("/api/v1/models"))).toBe(true);
    const document_link = dialog.getByRole("link", { name: "查看厂商文档", exact: true });
    await expect(document_link).not.toHaveAttribute("data-button-variant");
    const document_url = (await document_link.getAttribute("href"))!;
    const documentation = document_link.locator("..");
    await expect(documentation).toContainText("模板核查：");
    await document_link.scrollIntoViewIfNeeded();
    const link_box = await document_link.boundingBox();
    const note_box = await documentation.locator("span").boundingBox();
    expect(link_box!.y).toBeCloseTo(note_box!.y, 0);
    await dialog.screenshot({ path: test.info().outputPath("model-documentation-row.png") });
    await page.context().route(document_url, route => route.fulfill({ contentType: "text/html", body: "<p>厂商文档测试页</p>" }));
    const web_popup = page.waitForEvent("popup");
    await document_link.click();
    const opened_document = await web_popup;
    await expect(opened_document).toHaveURL(document_url);
    await opened_document.close();
    // 在同一页面替换原生桥，验证桌面链接委派命令且不导航；不实际启动用户浏览器。
    await page.evaluate(() => Object.assign(window, { isTauri: true, __TAURI_INTERNALS__: {
      invoke: async (command: string, args: { url: string }) => {
        if (command !== "open_external_http_url") throw new Error(`Unexpected native command: ${command}`);
        document.documentElement.dataset.openedDocument = args.url;
      },
    } }));
    const current_url = page.url();
    await document_link.click();
    await expect(page.locator("html")).toHaveAttribute("data-opened-document", document_url);
    expect(page.url()).toBe(current_url);
    expect(page.context().pages()).toHaveLength(1);
    await page.evaluate(() => {
      Reflect.deleteProperty(window, "isTauri");
      Reflect.deleteProperty(window, "__TAURI_INTERNALS__");
    });

    await dialog.getByRole("button", { name: "返回", exact: true }).click();
    await dialog.getByRole("button", { name: "返回", exact: true }).click();
    await add("百炼套餐", "套餐映射验证");
    await dialog.getByLabel("服务地址", { exact: true }).fill(`http://127.0.0.1:${address.port}/compatible-mode/v1`);
    await dialog.getByRole("button", { name: "接口选项", exact: true }).click();
    await expect(dialog.getByLabel("模型列表接口", { exact: true })).toHaveValue("");
    await dialog.getByRole("button", { name: "保存服务商", exact: true }).click();
    for (const [model, context, output] of [
      ["qwen3.7-max", "1000000", "131072"], ["qwen3.7-plus", "1000000", "131072"],
      ["qwen3.6-flash", "1000000", "65536"], ["glm-5.2", "1048576", "131072"],
      ["deepseek-v4-pro", "1000000", "393216"], ["deepseek-v4-flash-0731", "1000000", "393216"],
    ]) {
      await dialog.getByRole("button", { name: `配置模型 ${model}`, exact: true }).click();
      await expect(dialog.getByLabel("上下文窗口（Token）", { exact: true })).toHaveValue(context);
      await expect(dialog.getByLabel("运行输出上限（Token）", { exact: true })).toHaveValue(output);
      if (model === "deepseek-v4-pro") await expect(dialog.getByLabel("low 线上值", { exact: true })).toHaveValue("");
      if (model === "glm-5.2") await expect(dialog.getByLabel("x_high 线上值", { exact: true })).toHaveValue("xhigh");
      await dialog.getByRole("button", { name: "返回", exact: true }).click();
    }
    await dialog.getByRole("button", { name: "配置模型 qwen3.8-max", exact: true }).click();
    await expect(dialog.getByLabel("上下文窗口（Token）", { exact: true })).toHaveValue("1000000");
    expect(paths.some(path => path === "/compatible-mode/v1/models")).toBe(true);
    await dialog.getByLabel("上下文窗口（Token）", { exact: true }).fill("1000001");
    // 直接保存厂商预填能力与档位，无需用户逐项补齐才能运行。
    await expect(dialog.getByLabel("x_high 线上值", { exact: true })).toHaveValue("xhigh");
    await dialog.getByRole("button", { name: "保存固定配置", exact: true }).click();
    await expect(dialog.getByRole("button", { name: "重置配置", exact: true })).toBeEnabled();
    await expect(dialog.getByText(/^来源：/)).toHaveCount(0);
    await dialog.getByRole("button", { name: "测试已保存配置", exact: true }).click();
    await expect.poll(() => calls.some(call => call.reasoning_effort === "xhigh")).toBe(true);
    expect(calls.some(call => call.enable_thinking === true && call.preserve_thinking === true)).toBe(true);
    await dialog.getByRole("button", { name: "刷新在线参考", exact: true }).click();
    await expect(dialog.getByRole("button", { name: "使用本次在线值", exact: true })).toBeVisible();
    await expect(dialog.getByLabel("上下文窗口（Token）", { exact: true })).toHaveValue("1000001");
    await dialog.getByRole("button", { name: "返回", exact: true }).click();
    await expect(dialog.getByRole("button", { name: "配置模型 qwen3.8-max", exact: true })).toContainText("已自定义");
    await expect(dialog.getByRole("button", { name: "管理固定配置", exact: true })).toHaveCount(0);
    fail_list = true;
    await dialog.getByRole("button", { name: "刷新", exact: true }).click();
    const offline = dialog.getByRole("button", { name: "编辑固定配置 qwen3.8-max", exact: true });
    await expect(offline).toContainText("本次未返回");
    await offline.click();
    await expect(dialog.getByLabel("x_high 线上值", { exact: true })).toHaveValue("xhigh");
    const reasoning_section = dialog.getByRole("button", { name: "思考设置", exact: true });
    const advanced_section = dialog.getByRole("button", { name: "高级能力", exact: true });
    await reasoning_section.click();
    await expect(dialog.getByLabel("x_high 线上值", { exact: true })).toHaveCount(0);
    for (const header of [reasoning_section, advanced_section]) {
      await expect(header).toHaveCSS("padding-left", "0px");
      await expect(header).toHaveCSS("border-top-width", "0px");
      await expect(header).toHaveCSS("height", "24px");
      await expect(header.locator("..")).toHaveCSS("margin-top", "0px");
      await expect(header.locator("..")).toHaveCSS("height", "24px");
    }
    await advanced_section.scrollIntoViewIfNeeded();
    await dialog.screenshot({ path: test.info().outputPath("model-sections-collapsed.png") });
    await advanced_section.focus();
    await advanced_section.press("Enter");
    await expect(dialog.getByRole("button", { name: "工具选择 auto", exact: true })).toBeVisible();
    await advanced_section.press("Space");
    await expect(dialog.getByRole("button", { name: "工具选择 auto", exact: true })).toHaveCount(0);
    await reasoning_section.click();
    await expect(dialog.getByLabel("x_high 线上值", { exact: true })).toHaveValue("xhigh");
    const default_effort = dialog.getByRole("button", { name: "默认思考强度", exact: true });
    const max_effort = dialog.getByLabel("max 线上值", { exact: true });
    await default_effort.scrollIntoViewIfNeeded();
    const default_bounds = await default_effort.boundingBox();
    const max_bounds = await max_effort.boundingBox();
    expect(default_bounds!.width).toBeCloseTo(max_bounds!.width, 0);
    expect(default_bounds!.y).toBeCloseTo(max_bounds!.y, 0);
    const field_label = default_effort.locator("..").locator(":scope > span");
    await expect(field_label).toHaveCSS("font-size", await max_effort.locator("..").evaluate(element => getComputedStyle(element).fontSize));
    await expect(field_label).toHaveCSS("font-weight", "600");
    await default_effort.click();
    await page.getByRole("option", { name: "x_high", exact: true }).click();
    await expect(default_effort).toContainText("x_high");
    await dialog.screenshot({ path: test.info().outputPath("model-effort-fields.png") });
    await page.setViewportSize({ width: 390, height: 844 });
    await dialog.screenshot({ path: test.info().outputPath("model-config-390.png") });
    expect(await dialog.evaluate(element => element.scrollWidth <= element.clientWidth)).toBe(true);
    fail_list = false;
    await dialog.getByRole("button", { name: "重置配置", exact: true }).click();
    await page.getByRole("dialog", { name: "重置固定配置？", exact: true }).getByRole("button", { name: "重置配置", exact: true }).click();
    await expect(dialog.getByLabel("上下文窗口（Token）", { exact: true })).toHaveValue("1000000");
    await expect(dialog.getByLabel("x_high 线上值", { exact: true })).toHaveValue("xhigh");
    await expect(dialog.getByRole("button", { name: "重置配置", exact: true })).toBeDisabled();
    await page.setViewportSize({ width: 1280, height: 900 });
    await dialog.getByRole("button", { name: "返回", exact: true }).click();
    await dialog.getByRole("button", { name: "返回", exact: true }).click();
    await add("DeepSeek", "DeepSeek 模板验证");
    await dialog.getByRole("button", { name: "保存服务商", exact: true }).click();
    await expect(dialog.getByRole("heading", { name: "DeepSeek 模板验证", exact: true })).toBeVisible();
    // 从未进入详情／保存参数：直接选择默认与识图模型，再在真实会话发送。
    const direct_commands: string[] = [];
    page.on("request", request => {
      if (request.url().endsWith("/commands") && request.method() === "POST") {
        direct_commands.push(request.postDataJSON().command.payload.type as string);
      }
    });
    await dialog.getByRole("button", { name: "返回", exact: true }).click();
    for (const [purpose, model] of [["默认模型", "deepseek-v4-flash"], ["默认识图模型", "deepseek-v4-flash-vision-exp"]]) {
      await dialog.getByRole("button", { name: purpose, exact: true }).click();
      await expect(page.getByRole("menuitem", { name: "管理服务商", exact: true })).toHaveCount(0);
      await page.getByRole("menuitem", { name: /DeepSeek 模板验证/ }).click();
      await page.getByRole("menuitemradio", { name: model, exact: true }).click();
      await expect(dialog.getByRole("button", { name: purpose, exact: true })).toContainText(model);
    }
    await dialog.getByRole("button", { name: "关闭设置", exact: true }).click();
    await page.getByRole("region", { name: "主控会话", exact: true }).getByRole("button").click();
    await page.getByRole("button", { name: "模型设置", exact: true }).click();
    const default_option = page.getByRole("menuitemradio", { name: "默认模型", exact: true });
    await expect(default_option).toBeFocused();
    await expect(page.getByRole("menuitem", { name: "管理服务商", exact: true })).toBeVisible();
    await expect(default_option).not.toHaveAttribute("aria-haspopup", "menu");
    await page.keyboard.press("ArrowDown");
    await expect(page.locator("[data-setting-category-index=\"0\"]")).toBeFocused();
    await page.keyboard.press("Home");
    await expect(default_option).toBeFocused();
    await default_option.press("Enter");
    await page.getByRole("textbox", { name: "输入消息", exact: true }).fill("DIRECT_TEMPLATE_CHECK");
    await page.getByRole("button", { name: "发送消息", exact: true }).click();
    await expect(page.getByText("DIRECT_TEMPLATE_OK", { exact: true }).first()).toBeVisible();
    const numbered = page.locator('ol[data-streamdown="ordered-list"]');
    await expect(numbered).toHaveCount(4);
    for (const width of [1280, 720]) {
      await page.setViewportSize({ width, height: 900 });
      const lists = await numbered.evaluateAll(elements => elements.map(element => {
        const list = element as HTMLOListElement;
        const styles = getComputedStyle(list);
        const last = list.start + list.children.length - 1;
        const canvas = document.createElement("canvas");
        const context = canvas.getContext("2d")!;
        context.font = styles.font;
        return {
          padding: parseFloat(styles.paddingInlineStart),
          marker_width: context.measureText(`${last}. `).width,
          fits: list.scrollWidth <= list.clientWidth,
          position: styles.listStylePosition,
        };
      }));
      for (const list of lists) {
        expect(list.padding).toBeGreaterThanOrEqual(list.marker_width);
        expect(list.fits).toBe(true);
        expect(list.position).toBe("outside");
      }
      await page.getByText("九位数编号", { exact: true }).scrollIntoViewIfNeeded();
      await page.screenshot({ path: test.info().outputPath(`markdown-numbered-${width}.png`) });
    }
    await page.setViewportSize({ width: 1280, height: 900 });

    expect(calls.some(call => call.model === "deepseek-v4-flash" && call.stream === true)).toBe(true);
    await page.getByRole("button", { name: "模型设置", exact: true }).click();
    await expect(default_option).toHaveAttribute("aria-checked", "true");
    const effort_category = page.getByRole("menuitem", { name: /推理强度/ });
    await expect(effort_category).toBeVisible();
    expect(await effort_category.evaluate(element => element.previousElementSibling?.getAttribute("role"))).toBe("separator");
    await effort_category.click();
    await expect(page.getByRole("menu", { name: "推理强度", exact: true })).toBeVisible();
    await page.screenshot({ path: test.info().outputPath("model-default-effort-separator.png") });
    await page.keyboard.press("Escape");
    await expect(effort_category).toBeFocused();
    await page.keyboard.press("Escape");
    expect(direct_commands).not.toContain("get_model_configuration");
    expect(direct_commands).not.toContain("save_model_fixed_config");
    await page.getByRole("button", { name: "设置", exact: true }).click();
    await dialog.getByRole("button", { name: "模型", exact: true }).click();
    const clear_default = dialog.getByRole("button", { name: "清除默认模型", exact: true });
    await expect(clear_default).toHaveText("");
    await page.mouse.move(0, 0);
    await expect(clear_default.locator("svg").last()).toHaveCSS("opacity", "0");
    await clear_default.hover();
    await expect(clear_default.locator("svg").last()).toHaveCSS("opacity", "1");
    await expect(clear_default.locator("svg").first()).toHaveCSS("opacity", "0");
    await dialog.screenshot({ path: test.info().outputPath("select-clear-hover.png") });
    await clear_default.click();
    await expect(dialog.getByRole("button", { name: "默认模型", exact: true })).toContainText("未配置");
    await expect(dialog.getByRole("button", { name: "默认模型", exact: true })).toBeFocused();
    await expect(clear_default).toHaveCount(0);
    await expect(dialog.getByRole("button", { name: "默认模型", exact: true })).toHaveAttribute("aria-expanded", "false");
    const clear_vision = dialog.getByRole("button", { name: "清除默认识图模型", exact: true });
    await page.keyboard.press("Tab");
    await page.keyboard.press("Tab");
    await expect(clear_vision).toBeFocused();
    await expect(clear_vision.locator("svg").last()).toHaveCSS("opacity", "1");
    await clear_vision.press("Enter");
    await expect(dialog.getByRole("button", { name: "默认识图模型", exact: true })).toContainText("未配置");
    await dialog.getByRole("button", { name: /管理服务商 DeepSeek 模板验证/ }).click();
    for (const model of ["deepseek-v4-flash", "deepseek-v4-pro", "deepseek-v4-flash-vision-exp"]) {
      await dialog.getByRole("button", { name: `配置模型 ${model}`, exact: true }).click();
      await expect(dialog.getByLabel("上下文窗口（Token）", { exact: true })).toHaveValue(model.endsWith("vision-exp") ? "1048576" : "1000000");
      await expect(dialog.getByLabel("运行输出上限（Token）", { exact: true })).toHaveValue("384000");
      await expect(dialog.locator("form label small, form span small")).toHaveCount(0);
      await expect(dialog.getByText(/^来源：/)).toHaveCount(0);
      await dialog.getByRole("button", { name: "返回", exact: true }).click();
    }
    await dialog.getByRole("button", { name: "返回", exact: true }).click();
    await add("OpenAI", "自定义路径验证");
    await dialog.getByRole("button", { name: "接口选项", exact: true }).click();
    await expect(dialog.getByLabel("模型列表接口", { exact: true })).toHaveValue("");
    await dialog.getByLabel("模型列表接口", { exact: true }).fill("/custom/models");
    await dialog.getByRole("button", { name: "保存服务商", exact: true }).click();
    await expect(dialog.getByRole("button", { name: "配置模型 gpt-4.1", exact: true })).toBeVisible();
    expect(paths).toContain("/custom/models");
    await page.setViewportSize({ width: 390, height: 844 });
    await dialog.getByRole("button", { name: "关闭设置", exact: true }).click();
    await page.getByRole("button", { name: "模型设置", exact: true }).click();
    await expect(page.getByRole("menuitem", { name: /套餐映射验证/ })).toBeVisible();
    await expect(page.getByRole("menuitem", { name: "模型", exact: true })).toHaveCount(0);
    await page.getByRole("menuitem", { name: /套餐映射验证/ }).click();
    await expect(page.getByRole("menuitemradio", { name: "qwen3.8-max", exact: true })).toBeVisible();
    const model_menu = page.getByRole("menu", { name: "套餐映射验证", exact: true });
    await expect(model_menu.locator("strong")).toHaveCount(0);
    await expect(model_menu.getByRole("button", { name: "刷新", exact: true })).toHaveCount(0);
    await expect(page.getByRole("button", { name: "返回一级", exact: true })).toBeVisible();
    await page.screenshot({ path: test.info().outputPath("model-cascade-390.png") });
    await page.getByRole("button", { name: "返回一级", exact: true }).click();
    await expect(page.getByRole("menuitem", { name: /套餐映射验证/ })).toBeFocused();
    const fetch_count = paths.filter(path => path === "/compatible-mode/v1/models").length;
    await page.getByRole("menuitem", { name: /套餐映射验证/ }).click();
    await expect.poll(() => paths.filter(path => path === "/compatible-mode/v1/models").length).toBeGreaterThan(fetch_count);
    await expect(page.getByRole("menuitemradio", { name: "qwen3.8-max", exact: true })).toBeVisible();
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBe(true);
  } finally { server.closeAllConnections(); await new Promise<void>(resolve => server.close(() => resolve())); }
});
