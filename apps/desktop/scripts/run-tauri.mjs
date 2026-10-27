import { existsSync, readFileSync, writeFileSync, mkdirSync } from "node:fs";
import { createHash } from "node:crypto";
import { execFileSync, spawn } from "node:child_process";
import { homedir } from "node:os";
import { join } from "node:path";
import { env, platform } from "node:process";

import { selectMacDeveloperDirectory } from "./mac-developer-directory.mjs";

selectMacDeveloperDirectory("tauri");

const arguments_ = process.argv.slice(2);
if (arguments_[0] === "dev") {
  // 用户指定开发环境使用现有 Runtime 数据；显式覆盖仍可用于隔离测试。
  env.EZ_ASSISTANT_RUNTIME_HOME ??= join(homedir(), ".ez-assistant");
  await run(platform === "win32" ? "npm.cmd" : "npm", ["run", "build:host"]);
  // 独立标识隔离 WebView 存储、窗口状态和偏好；开发配置不覆盖 windows 数组，保留平台标题栏。
  arguments_.push("--config", "src-tauri/tauri.dev.conf.json");
}
if (arguments_[0] === "build") {
  const targetIndex = arguments_.indexOf("--target");
  const target = targetIndex < 0 ? arguments_.find((value) => value.startsWith("--target="))?.slice(9) : arguments_[targetIndex + 1];
  const targetArguments = target ? ["--target", target] : [];
  // ring needs clang on PATH for Windows ARM64; aws-lc also uses clang-cl.
  // Keep the discovery local to this build, without changing the user's system PATH.
  if (process.platform === "win32" && target === "aarch64-pc-windows-msvc") {
    const vswhere = join(process.env["ProgramFiles(x86)"], "Microsoft Visual Studio/Installer/vswhere.exe");
    const installation = execFileSync(vswhere, ["-latest", "-products", "*", "-requires", "Microsoft.VisualStudio.Component.VC.Llvm.Clang", "-property", "installationPath"], { encoding: "utf8", windowsHide: true }).trim();
    const llvm = join(installation, "VC/Tools/Llvm/x64/bin");
    if (!installation || !existsSync(join(llvm, "clang.exe"))) {
      throw new Error("Windows ARM64 requires Visual Studio C++ Clang Compiler for Windows and MSVC ARM64 tools.");
    }
    const pathKey = Object.keys(env).find((key) => key.toLowerCase() === "path") ?? "PATH";
    env[pathKey] = `${llvm};${env[pathKey] ?? ""}`;
  }
  await run(process.execPath, ["scripts/build-host.mjs", "--release", ...targetArguments]);
  await run(process.execPath, ["scripts/prepare-sidecar.mjs", ...targetArguments]);
  arguments_.push("--config", "src-tauri/tauri.bundle.conf.json");
  if (target ? /apple-darwin|windows-msvc/.test(target) : ["darwin", "win32"].includes(platform)) {
    arguments_.push("--config", "src-tauri/tauri.capture.conf.json");
  }
}
if (platform === "win32" && ["build", "bundle"].includes(arguments_[0])) {
  // 含文件和 HKCU KeyPath 的组件不能由 WiX 自动生成 GUID。按最终应用标识生成稳定 GUID，
  // 防止正式版与 Dev 安装共享组件身份；保留 Tauri 对模板其余字段的正式渲染。
  let identifier = JSON.parse(readFileSync("src-tauri/tauri.conf.json", "utf8")).identifier;
  for (let index = 1; index < arguments_.length; index += 1) {
    let config;
    if (arguments_[index] === "--config" || arguments_[index] === "-c") {
      config = arguments_[++index];
    } else if (arguments_[index].startsWith("--config=")) {
      config = arguments_[index].slice(9);
    }
    if (config) {
      const overlay = JSON.parse(config.trimStart().startsWith("{") ? config : readFileSync(config, "utf8").replace(/^\uFEFF/, ""));
      identifier = overlay.identifier ?? identifier;
    }
  }
  const template = readFileSync("src-tauri/wix/main.wxs", "utf8")
    .replace("__RUNTIME_TOOLS_EXECUTABLE_GUID__", componentGuid(identifier, "runtime-tools-executable"))
    .replace("__RUNTIME_TOOLS_LICENSES_GUID__", componentGuid(identifier, "runtime-tools-licenses"));
  mkdirSync("src-tauri/binaries", { recursive: true });
  writeFileSync("src-tauri/binaries/msi-main.wxs", template);
  writeFileSync("src-tauri/binaries/msi-config.json", JSON.stringify({ bundle: { windows: { wix: { template: "binaries/msi-main.wxs" } } } }));
  arguments_.push("--config", "src-tauri/binaries/msi-config.json");
}
await run(platform === "win32" ? "tauri.cmd" : "tauri", arguments_);

function componentGuid(identifier, role) {
  // RFC 9562 UUIDv5，DNS namespace；同一产品/组件跨版本保持，其他产品标识分别生成。
  const bytes = createHash("sha1")
    .update(Buffer.from("6ba7b8109dad11d180b400c04fd430c8", "hex"))
    .update(`${identifier}/msi/${role}`).digest().subarray(0, 16);
  bytes[6] = (bytes[6] & 0x0f) | 0x50;
  bytes[8] = (bytes[8] & 0x3f) | 0x80;
  const value = bytes.toString("hex");
  return `${value.slice(0, 8)}-${value.slice(8, 12)}-${value.slice(12, 16)}-${value.slice(16, 20)}-${value.slice(20)}`;
}

function run(command, arguments_) {
  return new Promise((resolve, reject) => {
    const child = spawn(command, arguments_, { stdio: "inherit", shell: command.endsWith(".cmd") });
    child.once("error", reject);
    child.once("exit", (code) => {
      if (code === 0) {
        resolve();
      } else {
        reject(new Error(`${command} exited with ${code ?? "unknown"}`));
      }
    });
  });
}
