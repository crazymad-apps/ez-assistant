// Build local release artifacts only; publishing is an explicit separate action.
import "./check-version.mjs";
import { spawn } from "node:child_process";
import { createHash } from "node:crypto";
import { copyFile, mkdir, readFile, readdir, stat, writeFile } from "node:fs/promises";
import { fileURLToPath } from "node:url";
import { resolve } from "node:path";

if (process.platform !== "win32") throw new Error("Windows MSI builds require Windows and Visual Studio C++ tools.");
const arguments_ = process.argv.slice(2);
if (arguments_.length !== 0 && (arguments_.length !== 2 || arguments_[0] !== "--arch" || !["x64", "arm64"].includes(arguments_[1]))) {
  throw new Error("Usage: release-windows.mjs [--arch x64|arm64] (default: both)");
}
const desktopRoot = fileURLToPath(new URL("../", import.meta.url));
const workspaceRoot = resolve(desktopRoot, "../..");
const targetRoot = resolve(workspaceRoot, process.env.CARGO_TARGET_DIR ?? "target");
const { version, productName } = JSON.parse(await readFile(resolve(desktopRoot, "src-tauri/tauri.conf.json"), "utf8"));
const output = resolve(targetRoot, "windows-release", version);
await mkdir(output, { recursive: true });
const architectures = arguments_[1] ? [arguments_[1]] : ["x64", "arm64"];
for (const architecture of architectures) {
  const triple = architecture === "x64" ? "x86_64-pc-windows-msvc" : "aarch64-pc-windows-msvc";
  const started = Date.now();
  await run(process.execPath, ["scripts/run-tauri.mjs", "build", "--target", triple, "--bundles", "msi"]);
  const source = resolve(targetRoot, triple, "release/bundle/msi", `${productName}_${version}_${architecture}_en-US.msi`);
  if ((await stat(source)).mtimeMs < started - 2000) throw new Error(`Refusing stale MSI: ${source}`);
  await run("powershell.exe", ["-NoProfile", "-ExecutionPolicy", "Bypass", "-File", "scripts/verify-windows-release.ps1", "-Msi", source, "-Architecture", architecture, "-Version", version]);
  const filename = `EZ-Assistant_${version}_windows_${architecture}.msi`;
  await copyFile(source, resolve(output, filename));
  const digest = createHash("sha256").update(await readFile(source)).digest("hex");
  await writeFile(resolve(output, `${filename}.sha256`), `${digest}  ${filename}\n`);
  console.log(`Created ${resolve(output, filename)} (SHA-256: ${digest})`);
}
const files = (await readdir(output)).filter((name) => name.endsWith(".msi"));
await writeFile(resolve(output, "RELEASE.md"), `# EZ Assistant ${version} — Windows\n\n` +
  `本目录包含本次构建产物；发布前须完成安装和运行验收。\n\n` +
  `- x64：Windows 10 1809 或更高版本。\n- arm64：Windows 11 ARM，原生 Desktop 与同架构 Runtime Host。\n` +
  `- 当前用户安装；卸载仅移除程序和快捷方式，保留 Runtime Home 与桌面偏好。\n` +
  `- 首版 MSI 与可执行文件未签名，Windows SmartScreen 可能提示未知发布者。\n` +
  `- 需要 WebView2；尚未安装时引导联网下载，失败后需安装 WebView2 再重试。\n` +
  `- 不安装 Windows Client、npm 包或开机自启任务。\n\n` +
  `附件（GitHub Release 发布时须包含两种架构及各自 SHA-256）：\n\n` +
  files.sort().map((name) => `- ${name}\n- ${name}.sha256\n`).join(""));

function run(command, arguments_) {
  return new Promise((accept, reject) => {
    const child = spawn(command, arguments_, { cwd: desktopRoot, env: process.env, stdio: "inherit", shell: false });
    child.once("error", reject);
    child.once("exit", (code) => code === 0 ? accept() : reject(new Error(`${command} failed: ${code}`)));
  });
}
