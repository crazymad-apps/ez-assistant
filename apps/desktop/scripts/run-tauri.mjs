import { spawn } from "node:child_process";
import { homedir } from "node:os";
import { join } from "node:path";
import { env, platform } from "node:process";

import { selectMacDeveloperDirectory } from "./mac-developer-directory.mjs";

selectMacDeveloperDirectory("tauri");

const arguments_ = process.argv.slice(2);
if (arguments_[0] === "dev") {
  // 用户指定开发环境使用现有 Runtime 数据；显式覆盖仍可用于隔离测试。
  env.EZ_ASSISTANT_RUNTIME_HOME ??= join(homedir(), ".ez-assistant");
  await run("npm", ["run", "build:host"]);
  // 独立标识隔离 WebView 存储、窗口状态和偏好；开发配置不覆盖 windows 数组，保留平台标题栏。
  arguments_.push("--config", "src-tauri/tauri.dev.conf.json");
}
if (arguments_[0] === "build") {
  await run("npm", ["run", "prepare:sidecar"]);
  arguments_.push("--config", "src-tauri/tauri.bundle.conf.json");
}
await run(platform === "win32" ? "tauri.cmd" : "tauri", arguments_);

function run(command, arguments_) {
  return new Promise((resolve, reject) => {
    const child = spawn(command, arguments_, { stdio: "inherit", shell: false });
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
