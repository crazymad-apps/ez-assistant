import { expect, test } from "@playwright/test";

test("switches formal terminal profiles and exposes bounded diagnostics", async ({ page }) => {
  await page.goto("/");

  await expect(page.getByRole("heading", { name: "智能终端" })).toBeVisible();
  await expect(page.getByRole("heading", { name: "设备与 Host" })).toBeVisible();
  await expect(page.getByRole("heading", { name: "交互形态" })).toBeVisible();
  await expect(page.getByRole("heading", { name: "当前轮次" })).toBeVisible();
  await expect(page.getByText("会话监控", { exact: true })).toBeVisible();

  const profile = page.getByLabel("终端形态");
  await expect(profile).toHaveValue("mixed");
  await profile.selectOption("keyboard_screen");
  await expect(page.locator("#preference")).toHaveValue("text");
  await expect(page.getByRole("button", { name: "开始录音" })).toBeDisabled();
  await expect(page.getByRole("button", { name: "发送", exact: true })).toBeEnabled();
  await expect(page.getByRole("button", { name: "启用播报" })).toBeDisabled();

  await profile.selectOption("voice_only");
  await expect(page.locator("#preference")).toHaveValue("audio");
  await expect(page.getByRole("button", { name: "发送", exact: true })).toBeDisabled();
  await expect(page.getByRole("button", { name: "开始录音" })).toBeEnabled();
  await expect(page.getByRole("button", { name: "启用播报" })).toBeEnabled();

  const faultPanel = page.locator("details.fault-panel");
  const diagnostics = page.locator("details.diagnostics");
  await expect(faultPanel).not.toHaveAttribute("open", "");
  await expect(diagnostics).not.toHaveAttribute("open", "");
  await faultPanel.getByText("协议故障注入", { exact: true }).click();
  await page.locator("#fault").selectOption("duplicate_next_text_envelope");
  await page.getByRole("button", { name: "注入" }).click();
  await diagnostics.getByText("会话监控", { exact: true }).click();
  await expect(page.locator("#armed-fault")).toContainText("duplicate_next_text_envelope");
  await expect(page.locator("#events")).toContainText("fault_armed");
});
