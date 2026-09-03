import { expect, type Page } from "@playwright/test";

/** 正式 Host 浏览器回证；只使用全局 setup 创建的临时会话和安全 MCP fixture。 */
export async function expectMcpAcceptance(page: Page): Promise<void> {
  const secret = process.env.EZ_ASSISTANT_E2E_MCP_SECRET;
  if (!secret) throw new Error("isolated MCP credential marker is missing");
  await page.getByRole("button", { name: "设置", exact: true }).click();
  const settings = page.getByRole("dialog", { name: "设置" });
  await settings.getByRole("button", { name: "MCP", exact: true }).click();
  await settings.getByRole("button", { name: /MCP fixture local_fixture/ }).click();
  await expect(settings.getByLabel("显示名称")).toHaveValue("MCP fixture");
  await expect(settings.getByRole("button", { name: "删除服务…" })).toBeVisible();
  await expectCleanPage(page, secret);
  await settings.getByRole("button", { name: "测试连接", exact: true }).click();
  await expect(settings.getByText(/连接测试成功.*2 个工具/)).toBeVisible();
  await expectCleanPage(page, secret);
  // 长于原 10 分钟上限的覆盖可保存并回显；清空后恢复继承，不写成零。
  await settings.getByText("高级设置", { exact: true }).click();
  await settings.getByLabel("工具超时（毫秒）", { exact: true }).fill("1800000");
  await settings.getByRole("button", { name: "保存", exact: true }).click();
  await settings.getByRole("button", { name: /MCP fixture local_fixture/ }).click();
  await settings.getByText("高级设置", { exact: true }).click();
  await expect(settings.getByLabel("工具超时（毫秒）", { exact: true })).toHaveValue("1800000");
  await settings.getByLabel("工具超时（毫秒）", { exact: true }).fill("");
  await settings.getByRole("button", { name: "保存", exact: true }).click();
  await settings.getByRole("button", { name: /MCP fixture local_fixture/ }).click();
  await settings.getByText("高级设置", { exact: true }).click();
  await expect(settings.getByLabel("工具超时（毫秒）", { exact: true })).toHaveValue("");
  await settings.getByRole("button", { name: "返回 MCP 列表" }).click();
  await settings.getByRole("button", { name: "关闭设置" }).click();

  // 单次允许不能扩大为服务级权限；下一轮调用必须再次出现审批。
  const composer = page.getByRole("textbox", { name: "输入消息" });
  for (const allow of [true, false]) {
    await composer.fill("/mcp");
    await composer.press("Enter");
    await page.getByRole("option", { name: /MCP fixture \(local_fixture\)/ }).click();
    await composer.fill("MCP_CASE");
    await composer.press("Enter");
    await expect(page.getByText("允许调用 local_fixture / first_tool？", { exact: true })).toBeVisible();
    await expectCleanPage(page, secret);
    if (allow) {
      await page.getByRole("radio", { name: /仅本次/ }).check();
      await page.getByRole("button", { name: "允许执行", exact: true }).click();
      await expect(page.getByText("MCP 调用已回传。", { exact: true })).toBeVisible();
      await page.getByRole("button", { name: /MCP fixture.*first_tool.*完成/ }).last().click();
      const detail = page.getByRole("dialog", { name: "MCP fixture (local_fixture) / first_tool" });
      await expect(detail).toContainText("called:first_tool");
      await expectCleanPage(page, secret);
      await detail.getByRole("button", { name: "关闭工具详情" }).click();
    } else {
      await page.getByRole("button", { name: "拒绝", exact: true }).click();
      await expect(page.getByText("MCP 调用已拒绝。", { exact: true })).toBeVisible();
    }
  }

  await composer.fill("/mcp refresh");
  await composer.press("Enter");
  const result = page.getByRole("article", { name: "MCP 刷新完成" });
  await expect(result).toBeVisible();
  await result.locator("summary").click();
  await expect(result).toContainText("已刷新 · 2 个工具");
  await expectCleanPage(page, secret);
  await page.reload();
  await expect(page.getByRole("article", { name: "MCP 刷新完成" })).toBeVisible();
  await expectCleanPage(page, secret);
}

async function expectCleanPage(page: Page, secret: string): Promise<void> {
  // 不只扫描 outerHTML：输入值可能只存在 DOM property，浏览器存储也不能残留凭据。
  const exposed = await page.evaluate(() => JSON.stringify({
    html: document.documentElement.outerHTML,
    values: [...document.querySelectorAll("input,textarea")].map((element) => (element as HTMLInputElement).value),
    local: { ...localStorage }, session: { ...sessionStorage },
  }));
  expect(exposed).not.toContain(secret);
  expect(exposed).not.toContain("e2e-placeholder-not-a-real-secret");
}
