import { spawn } from "node:child_process";
import { fileURLToPath } from "node:url";

const desktop = fileURLToPath(new URL("../", import.meta.url));
const environment = { ...process.env, EZ_ASSISTANT_WEB_DIST: fileURLToPath(new URL("../dist", import.meta.url)) };
const npm = process.platform === "win32" ? "npm.cmd" : "npm";
await run(["run", "build"]);
await run(["run", "cargo", "--", "build", "-p", "assistant-runtime-host", ...process.argv.slice(2)]);

function run(arguments_) {
  return new Promise((resolve, reject) => {
    const child = spawn(npm, arguments_, { cwd: desktop, env: environment, stdio: "inherit", shell: false });
    child.once("error", reject);
    child.once("exit", (code) => code === 0 ? resolve() : reject(new Error(`Host build step exited with ${code ?? "unknown"}`)));
  });
}
