// 独立 Host 平台能力构建；Desktop/Client 只复制结果，不反向决定原生实现。
import { execFileSync } from "node:child_process";
import { mkdir, mkdtemp, writeFile, rm } from "node:fs/promises";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const [outputArgument, targetArgument] = process.argv.slice(2);
if (!outputArgument) throw new Error("Usage: build-desktop-capture.mjs <output-executable> [target-triple]");
const output = resolve(outputArgument);
const root = fileURLToPath(new URL("../", import.meta.url));
await mkdir(dirname(output), { recursive: true });
const run = (file, args, options = {}) => execFileSync(file, args, { stdio: "inherit", ...options });
if (process.platform === "darwin") {
  const target = targetArgument ?? (process.arch === "arm64" ? "aarch64-apple-darwin" : "x86_64-apple-darwin");
  if (!["aarch64-apple-darwin", "x86_64-apple-darwin"].includes(target)) throw new Error("Native macOS capture build requires a macOS target");
  const sdk = execFileSync("xcrun", ["--sdk", "macosx", "--show-sdk-path"], { encoding: "utf8" }).trim();
  run("xcrun", ["swiftc", "-O", "-sdk", sdk, "-target", `${target.startsWith("aarch64") ? "arm64" : "x86_64"}-apple-macos11.0`, join(root, "native/desktop/macos/main.swift"), join(root, "native/desktop/macos/Input.swift"), "-o", output]);
  // 开发默认 ad-hoc；正式打包传入同一 Developer ID，始终保留稳定 helper 标识。
  run("codesign", ["--force", "--options", "runtime", "--identifier", "com.ez-assistant.desktop.capture",
    "--sign", process.env.APPLE_SIGNING_IDENTITY || "-", output]);
} else if (process.platform === "win32") {
  const target = targetArgument ?? (process.arch === "arm64" ? "aarch64-pc-windows-msvc" : "x86_64-pc-windows-msvc");
  if (!["x86_64-pc-windows-msvc", "aarch64-pc-windows-msvc"].includes(target)) throw new Error("Unsupported Windows capture target");
  const vswhere = join(process.env["ProgramFiles(x86)"] ?? "C:/Program Files (x86)", "Microsoft Visual Studio/Installer/vswhere.exe");
  const vs = execFileSync(vswhere, ["-latest", "-products", "*", "-requires", "Microsoft.VisualStudio.Component.VC.Tools.x86.x64", "-property", "installationPath"], { encoding: "utf8" }).trim();
  if (!vs) throw new Error("Visual Studio C++ build tools are required for desktop capture");
  const work = await mkdtemp(join(dirname(output), "desktop-capture-build-"));
  // cmd 文件只接受经过校验的本机构建路径，禁止扩展元字符造成替换或重定向。
  const quote = (path) => { if (/[\r\n%"&|<>^]/.test(path)) throw new Error("Unsupported command metacharacter in build path"); return `"${path}"`; };
  try {
    const batch = join(work, "build.cmd");
    const source = join(root, "native/desktop/windows/main.cpp");
    await writeFile(batch, `@echo off\r\ncall ${quote(join(vs, "Common7/Tools/VsDevCmd.bat"))} -no_logo -arch=${target.startsWith("aarch64") ? "arm64" : "x64"} -host_arch=x64\r\nif errorlevel 1 exit /b %ERRORLEVEL%\r\ncl.exe /nologo /utf-8 /std:c++17 /EHsc /O2 /MT /W4 /Fo${quote(join(work, "capture.obj"))} /Fe${quote(output)} ${quote(source)} /link d3d11.lib dxgi.lib mfplat.lib mf.lib mfuuid.lib wmcodecdspuuid.lib ole32.lib oleaut32.lib user32.lib\r\nexit /b %ERRORLEVEL%\r\n`);
    run("cmd.exe", ["/d", "/c", batch]);
  } finally { await rm(work, { recursive: true, force: true }); }
} else {
  throw new Error("Desktop capture targets are macOS and Windows only");
}
