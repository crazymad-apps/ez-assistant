import { spawn } from "node:child_process";
import { fileURLToPath } from "node:url";

const desktop = fileURLToPath(new URL("../", import.meta.url));
const environment = { ...process.env, EZ_ASSISTANT_WEB_DIST: fileURLToPath(new URL("../dist", import.meta.url)) };
const npm = process.platform === "win32" ? "npm.cmd" : "npm";
await run(["run", "build"]);
// 随包 Windows Host 不要求目标机器另外安装 VC++ Redistributable。
// 只作用于 Host Release 构建，不影响前端协议生成和用户的系统环境。
if (process.platform === "win32" && process.argv.includes("--release")) {
  if (environment.CARGO_ENCODED_RUSTFLAGS !== undefined) {
    environment.CARGO_ENCODED_RUSTFLAGS = [environment.CARGO_ENCODED_RUSTFLAGS, "-C", "target-feature=+crt-static"].filter(Boolean).join("\x1f");
  } else {
    environment.RUSTFLAGS = `${environment.RUSTFLAGS ?? ""} -C target-feature=+crt-static`.trim();
  }
}
await run(["run", "cargo", "--", "build", "-p", "assistant-runtime-host", ...process.argv.slice(2)]);

function run(arguments_) {
  return new Promise((resolve, reject) => {
    const child = spawn(npm, arguments_, { cwd: desktop, env: environment, stdio: "inherit", shell: npm.endsWith(".cmd") });
    child.once("error", reject);
    child.once("exit", (code) => code === 0 ? resolve() : reject(new Error(`Host build step exited with ${code ?? "unknown"}`)));
  });
}
