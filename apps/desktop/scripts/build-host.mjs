import { spawn } from "node:child_process";
import { fileURLToPath } from "node:url";
import { resolve } from "node:path";
import { nativeTarget, prepareRuntimeTools } from "../../runtime-host/scripts/prepare-runtime-tools.mjs";
import { selectMacDeveloperDirectory } from "./mac-developer-directory.mjs";

selectMacDeveloperDirectory("desktop-capture");

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
if (process.platform === "darwin" || process.platform === "win32") {
  const args = process.argv.slice(2);
  const index = args.indexOf("--target");
  const target = index < 0 ? args.find((value) => value.startsWith("--target="))?.slice(9) : args[index + 1];
  const output = resolve(desktop, "../..", process.env.CARGO_TARGET_DIR ?? "target", target ?? "",
    args.includes("--release") ? "release" : "debug", `ez-assistant-desktop-capture${process.platform === "win32" ? ".exe" : ""}`);
  await new Promise((resolve, reject) => {
    const child = spawn(process.execPath, [fileURLToPath(new URL("../../runtime-host/scripts/build-desktop-capture.mjs", import.meta.url)), output, ...(target ? [target] : [])], { env: environment, stdio: "inherit" });
    child.once("error", reject);
    child.once("exit", (code) => code === 0 ? resolve() : reject(new Error(`Desktop capture build exited with ${code}`)));
  });
}

const hostArgs = process.argv.slice(2);
const targetIndex = hostArgs.indexOf("--target");
const selectedTarget = targetIndex < 0 ? hostArgs.find((value) => value.startsWith("--target="))?.slice(9) : hostArgs[targetIndex + 1];
const targetRoot = resolve(desktop, "../..", process.env.CARGO_TARGET_DIR ?? "target");
await prepareRuntimeTools(resolve(targetRoot, selectedTarget ?? "", hostArgs.includes("--release") ? "release" : "debug"),
  selectedTarget ?? nativeTarget(), resolve(targetRoot, "runtime-tools-cache"));

function run(arguments_) {
  return new Promise((resolve, reject) => {
    const child = spawn(npm, arguments_, { cwd: desktop, env: environment, stdio: "inherit", shell: npm.endsWith(".cmd") });
    child.once("error", reject);
    child.once("exit", (code) => code === 0 ? resolve() : reject(new Error(`Host build step exited with ${code ?? "unknown"}`)));
  });
}
