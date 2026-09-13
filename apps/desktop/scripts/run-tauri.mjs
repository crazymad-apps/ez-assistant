import { existsSync } from "node:fs";
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
}
await run(platform === "win32" ? "tauri.cmd" : "tauri", arguments_);

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
