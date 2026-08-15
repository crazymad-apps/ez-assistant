import { spawn } from "node:child_process";
import { platform } from "node:process";

const arguments_ = process.argv.slice(2);
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
