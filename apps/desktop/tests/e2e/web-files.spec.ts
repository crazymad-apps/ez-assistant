import { compatibilityHeaders } from "@ez-assistant/protocol";
import { expect, test, type Page } from "@playwright/test";
import { mkdir, writeFile, readFile } from "node:fs/promises";
import { join, basename } from "node:path";

type Fixture = { base_url: string; access_token: string };
async function login(page: Page) {
  const host = JSON.parse(process.env.EZ_ASSISTANT_E2E_BOOTSTRAP!) as Fixture;
  const response = await fetch(`${host.base_url}/auth/login`, {
    method: "POST",
    headers: { ...compatibilityHeaders(), Authorization: `Bearer ${host.access_token}`,
      "Content-Type": "application/json",
    },
    body: JSON.stringify({ method: "desktop" }),
  });
  expect(response.status).toBe(200);
  const { token } = (await response.json()) as { token: string };
  await page.goto(`${host.base_url}/#token=${encodeURIComponent(token)}`);
  await expect(page.locator("[data-app-title-bar]")).toBeVisible();
  expect(new URL(page.url()).hash).toBe("");
}

test("Web selects an unregistered Host directory, sends browser files, previews and downloads Host content, then restores view", async ({
  page,
}, info) => {
  const directory = process.env.EZ_ASSISTANT_E2E_NEW_WORKSPACE!;
  await mkdir(join(directory, "子目录"));
  await mkdir(join(directory, ".hidden"));
  await writeFile(join(directory, "同名.txt"), "HOST FILE CONTENT");
  await writeFile(
    join(directory, "readme.md"),
    "# Host 文件预览\n\nHOST_MARKDOWN_ONLY\n\n[相邻文件](./同名.txt)\n",
  );
  const errors: string[] = [];
  page.on("pageerror", (error) => errors.push(error.message));
  await login(page);
  await page.getByRole("button", { name: "添加工作空间", exact: true }).click();
  const picker = page.getByRole("dialog", {
    name: "选择工作目录",
    exact: true,
  });
  await expect(picker).toBeVisible();
  await picker.getByLabel("Host 目录路径").fill(directory);
  await picker.getByRole("button", { name: "前往", exact: true }).click();
  await expect(picker.getByRole("button", { name: "子目录" })).toBeVisible();
  await expect(
    picker.getByRole("button", { name: ".hidden", exact: true }),
  ).toHaveCount(0);
  await picker.getByRole("checkbox", { name: "显示隐藏目录" }).check();
  await expect(
    picker.getByRole("button", { name: ".hidden", exact: true }),
  ).toBeVisible();
  await picker.getByLabel("Host 目录路径").fill(join(directory, "missing"));
  await picker.getByRole("button", { name: "前往", exact: true }).click();
  await expect(picker.getByRole("alert")).toContainText("不存在");
  await picker.getByLabel("Host 目录路径").fill(directory);
  await picker.getByRole("button", { name: "前往", exact: true }).click();
  await expect(picker.getByRole("button", { name: "子目录" })).toBeVisible();
  await page.screenshot({ path: info.outputPath("host-directory-picker.png") });
  await picker.getByRole("button", { name: "选择此目录", exact: true }).click();
  const editor = page.getByRole("dialog", {
    name: "新建工作空间",
    exact: true,
  });
  await expect(editor).toBeVisible();
  await editor.getByRole("textbox").fill("M3 Web 文件");
  await editor.getByRole("button", { name: "保存", exact: true }).click();
  await expect(editor).toHaveCount(0);
  await page
    .getByRole("button", { name: "M3 Web 文件 工作空间操作", exact: true })
    .click();
  await page.getByRole("menuitem", { name: "在此新建会话" }).click();
  await expect(
    page.getByRole("menuitem", { name: "打开工作目录" }),
  ).toHaveCount(0);
  const chooser = page.waitForEvent("filechooser");
  await page.getByRole("button", { name: "添加附件", exact: true }).click();
  await (
    await chooser
  ).setFiles({
    name: "同名.txt",
    mimeType: "text/plain",
    buffer: Buffer.from("BROWSER FILE CONTENT"),
  });
  await expect(
    page.getByRole("button", { name: "查看附件 同名.txt", exact: true }),
  ).toBeVisible();
  await page
    .getByLabel("输入消息", { exact: true })
    .fill("M3_BROWSER_FILE_UPLOAD");
  const materialized = page.waitForResponse(
    (r) =>
      r.url().endsWith("/session-materializations") &&
      r.request().method() === "POST",
  );
  await page.getByRole("button", { name: "发送消息", exact: true }).click();
  const response = await materialized;
  expect(response.status()).toBe(200);
  const result = (await response.json()) as {
    session: { session_id: string };
    attachments: { attachment_id: string }[];
  };
  const uploaded = await page.request.get(
    `${new URL(page.url()).origin}/sessions/${result.session.session_id}/attachments/${result.attachments[0].attachment_id}/download`,
  );
  expect(await uploaded.text()).toBe("BROWSER FILE CONTENT");
  await page.getByRole("button", { name: "新建资源标签", exact: true }).click();
  await expect(page.getByRole("menuitem", { name: "浏览器" })).toHaveCount(0);
  await page.getByRole("menuitem", { name: "工作空间", exact: true }).click();
  const tree = page.getByRole("tree", { name: "工作空间目录" });
  await tree
    .getByRole("button", { name: new RegExp(basename(directory) + ".*主目录") })
    .click();
  await tree.getByRole("button", { name: "readme.md", exact: true }).dblclick();
  await expect(
    page.getByText("HOST_MARKDOWN_ONLY", { exact: true }),
  ).toBeVisible();
  await page.getByRole("button", { name: "相邻文件", exact: true }).click();
  await expect(
    page.getByText("HOST FILE CONTENT", { exact: true }),
  ).toBeVisible();
  // 首次打开非 JSON 文件也必须加载 Monaco 图标字体，不能依赖其他语言功能预热。
  await page.getByRole("button", { name: "更多资源操作", exact: true }).click();
  await page.getByRole("menuitem", { name: "查找", exact: true }).click();
  const findWidget = page.locator(".monaco-editor .find-widget");
  await expect(findWidget).toBeVisible();
  const fonts = await page.evaluate(async () =>
    (await document.fonts.load("16px codicon")).map((font) => ({ family: font.family, status: font.status })),
  );
  expect(fonts).toContainEqual({ family: "codicon", status: "loaded" });
  await findWidget.screenshot({ path: info.outputPath("web-editor-find-widget.png") });
  await page.screenshot({ path: info.outputPath("web-editor-find-icons.png") });
  await findWidget.locator(".codicon-widget-close").click();
  await expect(findWidget).toHaveAttribute("aria-hidden", "true");
  await page.getByRole("button", { name: "更多资源操作", exact: true }).click();
  await expect(
    page.getByRole("menuitem", { name: "使用系统应用打开" }),
  ).toHaveCount(0);
  const downloading = page.waitForEvent("download");
  await page.getByRole("menuitem", { name: "下载文件", exact: true }).click();
  const download = await downloading;
  expect(download.suggestedFilename()).toBe("同名.txt");
  expect(await readFile((await download.path())!, "utf8")).toBe(
    "HOST FILE CONTENT",
  );
  await page.reload();
  await expect(
    page.getByText("HOST FILE CONTENT", { exact: true }),
  ).toBeVisible();
  const stored = await page.evaluate(() => JSON.stringify(localStorage));
  expect(stored).not.toContain("BROWSER FILE CONTENT");
  expect(stored).not.toContain("HOST FILE CONTENT");
  await page.screenshot({ path: info.outputPath("web-host-file-preview.png") });
  expect(errors).toEqual([]);
});

test("Web pastes an image into an existing session and previews the uploaded attachment", async ({
  page,
}, info) => {
  await login(page);
  await page.getByText("M2 临时会话", { exact: true }).first().click();
  await page
    .getByLabel("输入消息", { exact: true })
    .evaluate(async (element) => {
      const canvas = document.createElement("canvas");
      canvas.width = 128;
      canvas.height = 128;
      const context = canvas.getContext("2d")!;
      context.fillStyle = "#506aff";
      context.fillRect(0, 0, 128, 128);
      const blob = await new Promise<Blob>((resolve, reject) =>
        canvas.toBlob(
          (blob) =>
            blob ? resolve(blob) : reject(new Error("PNG fixture failed")),
          "image/png",
        ),
      );
      const clipboard = new DataTransfer();
      clipboard.items.add(
        new File([blob], "pasted.png", { type: "image/png" }),
      );
      element.dispatchEvent(
        new ClipboardEvent("paste", {
          clipboardData: clipboard,
          bubbles: true,
          cancelable: true,
        }),
      );
    });
  await expect(
    page.getByRole("button", { name: "查看附件 pasted.png", exact: true }),
  ).toBeVisible();
  const uploaded = page.waitForResponse(
    (r) =>
      /\/sessions\/[^/]+\/attachments$/.test(r.url()) &&
      r.request().method() === "POST",
  );
  await page.getByRole("button", { name: "发送消息", exact: true }).click();
  const response = await uploaded;
  expect(response.status()).toBe(200);
  await expect(
    page.getByRole("img", { name: "pasted.png", exact: true }).first(),
  ).toBeVisible();
  await page
    .getByRole("img", { name: "pasted.png", exact: true })
    .first()
    .click();
  await expect(page.getByRole("tab", { name: /pasted.png/ })).toBeVisible();
  await page.screenshot({ path: info.outputPath("web-pasted-image.png") });
});
