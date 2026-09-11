import { compatibilityHeaders } from "@ez-assistant/protocol";
import { expect, test } from "@playwright/test";
import { createServer } from "node:http";

// 隔离 Host + 模拟服务商，覆盖 UI 和真实持久化链路，不使用用户目录。
test("manual model origin survives online changes and controls deletion and tags", async ({ page }) => {
  test.setTimeout(90_000);
  let fail_list = false;
  let include_manual = false;
  let fetches = 0;
  let validations = 0;
  const server = createServer(async (request, response) => {
    if (request.method === "GET") {
      fetches++;
      response.setHeader("Content-Type", "application/json");
      if (fail_list) { response.writeHead(503).end("{}"); return; }
      response.end(JSON.stringify({ object: "list", data: ["deepseek-v4-pro", "unknown-model", ...(include_manual ? ["deepseek-v4-flash"] : [])].map(id => ({ id, object: "model" })) }));
      return;
    }
    let raw = ""; for await (const chunk of request) raw += chunk;
    const body = JSON.parse(raw) as { model: string };
    validations++;
    response.writeHead(200, { "Content-Type": "text/event-stream" });
    for (const delta of [{ role: "assistant" }, { content: "OK" }]) response.write(`data: ${JSON.stringify({ id: "manual-check", model: body.model, choices: [{ index: 0, delta }] })}\n\n`);
    response.end(`data: ${JSON.stringify({ id: "manual-check", model: body.model, choices: [{ index: 0, delta: {}, finish_reason: "stop" }], usage: { prompt_tokens: 1, completion_tokens: 1, total_tokens: 2 } })}\n\ndata: [DONE]\n\n`);
  });
  await new Promise<void>(resolve => server.listen(0, "127.0.0.1", resolve));
  const address = server.address();
  if (!address || typeof address === "string") throw new Error("fixture address missing");
  try {
    const fixture = JSON.parse(process.env.EZ_ASSISTANT_E2E_BOOTSTRAP!) as { base_url: string; access_token: string };
    const issued = await fetch(`${fixture.base_url}/auth/login`, { method: "POST", headers: { ...compatibilityHeaders(), Authorization: `Bearer ${fixture.access_token}`, "Content-Type": "application/json" }, body: JSON.stringify({ method: "desktop" }) });
    const { token } = await issued.json() as { token: string };
    await page.emulateMedia({ reducedMotion: "reduce" });
    await page.goto(`${fixture.base_url}/#token=${encodeURIComponent(token)}`);
    await page.getByRole("button", { name: "设置", exact: true }).click();
    const dialog = page.getByRole("dialog", { name: "设置", exact: true });
    await dialog.getByRole("button", { name: "模型", exact: true }).click();
    await dialog.getByRole("button", { name: "添加服务商", exact: true }).click();
    await dialog.getByRole("button", { name: "服务商类型", exact: true }).click();
    await page.getByRole("option", { name: "DeepSeek", exact: true }).click();
    await dialog.getByLabel("服务商名称", { exact: true }).fill("手动模型验证");
    await dialog.getByLabel("服务地址", { exact: true }).fill(`http://127.0.0.1:${address.port}/v1`);
    await dialog.getByLabel("API Key", { exact: true }).fill("artificial-manual-key");
    await dialog.getByRole("button", { name: "接口选项", exact: true }).click();
    await dialog.getByRole("button", { name: "接口协议", exact: true }).click();
    await page.getByRole("option", { name: "Chat Completions", exact: true }).click();
    await dialog.getByRole("button", { name: "保存服务商", exact: true }).click();
    const online = dialog.getByRole("button", { name: "配置模型 deepseek-v4-pro", exact: true });
    await expect(online).toContainText("模板预填");
    await expect(online).not.toContainText("待补全");
    await expect(dialog.getByRole("button", { name: "配置模型 unknown-model", exact: true })).toContainText("待补全");
    // 首次缺席于目录也能使用模板新增，草稿与保存均不查询目录。
    await dialog.screenshot({ path: test.info().outputPath("online-model-tags.png") });
    const before = fetches;
    await dialog.getByRole("button", { name: "添加模型", exact: true }).click();
    await dialog.getByLabel("模型 ID", { exact: true }).fill("deepseek-v4-flash");
    await dialog.getByRole("button", { name: "配置参数", exact: true }).click();
    await expect(dialog.getByLabel("上下文窗口（Token）", { exact: true })).not.toHaveValue("");
    await expect(dialog.getByLabel("运行输出上限（Token）", { exact: true })).not.toHaveValue("");
    await dialog.getByRole("button", { name: "保存固定配置", exact: true }).click();
    await expect(dialog.getByRole("button", { name: "删除模型", exact: true })).toBeEnabled();
    expect(fetches).toBe(before);
    await dialog.getByRole("button", { name: "测试已保存配置", exact: true }).click();
    await expect.poll(() => validations).toBeGreaterThan(0);
    await expect(dialog.getByRole("status")).toContainText("连接测试成功");
    await expect(dialog.getByRole("button", { name: "保存固定配置", exact: true })).toBeEnabled();
    fail_list = true;
    await dialog.getByRole("button", { name: "返回", exact: true }).click();
    const manual = dialog.getByRole("button", { name: "编辑固定配置 deepseek-v4-flash", exact: true });
    await expect(manual).toContainText("手动添加");
    await expect(manual).toContainText("已自定义");
    await dialog.screenshot({ path: test.info().outputPath("manual-model-tags.png") });
    await dialog.getByRole("button", { name: "返回", exact: true }).click();
    await dialog.getByRole("button", { name: "默认模型", exact: true }).click();
    await page.getByRole("menuitem", { name: /手动模型验证/ }).click();
    await expect(page.getByRole("menuitemradio", { name: /deepseek-v4-flash/ })).toBeVisible();
    await page.getByRole("menuitemradio", { name: /deepseek-v4-flash/ }).click();
    await expect(dialog.getByRole("button", { name: "默认模型", exact: true })).toContainText("deepseek-v4-flash");
    fail_list = false; include_manual = true;
    await dialog.getByRole("button", { name: /管理服务商 手动模型验证/ }).click();
    const merged = dialog.getByRole("button", { name: "配置模型 deepseek-v4-flash", exact: true });
    await expect(merged).toContainText("手动添加");
    await expect(dialog.getByRole("button", { name: /(?:配置模型|编辑固定配置) deepseek-v4-flash$/ })).toHaveCount(1);
    await merged.click();
    await expect(dialog.getByRole("button", { name: "重置配置", exact: true })).toHaveCount(0);
    await dialog.getByRole("button", { name: "删除模型", exact: true }).click();
    await page.getByRole("dialog", { name: "删除手动模型？", exact: true }).getByRole("button", { name: "删除模型", exact: true }).click();
    await expect(merged).toContainText("在线发现");
    await expect(merged).toContainText("模板预填");
    await expect(merged).not.toContainText("手动添加");
    await merged.click();
    await dialog.getByRole("button", { name: "保存固定配置", exact: true }).click();
    await expect(dialog.getByRole("button", { name: "重置配置", exact: true })).toBeEnabled();
    await expect(dialog.getByRole("button", { name: "删除模型", exact: true })).toHaveCount(0);
    await dialog.getByRole("button", { name: "返回", exact: true }).click();
    await dialog.getByRole("button", { name: "添加模型", exact: true }).click();
    await dialog.getByLabel("模型 ID", { exact: true }).fill("deepseek-v4-flash");
    await dialog.getByRole("button", { name: "配置参数", exact: true }).click();
    await expect(dialog.getByText("该模型已有配置，正在编辑原记录。", { exact: true })).toBeVisible();
    await dialog.getByRole("button", { name: "重置配置", exact: true }).click();
    await page.getByRole("dialog", { name: "重置固定配置？", exact: true }).getByRole("button", { name: "重置配置", exact: true }).click();
    await expect(dialog.getByRole("button", { name: "重置配置", exact: true })).toBeDisabled();
    await expect(dialog.getByRole("button", { name: "删除模型", exact: true })).toHaveCount(0);
  } finally {
    server.closeAllConnections();
    await new Promise<void>((resolve, reject) => server.close(error => error ? reject(error) : resolve()));
  }
});
