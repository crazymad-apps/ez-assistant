#!/bin/bash
set -euo pipefail

if [[ "${1:-}" == "--help" || "${1:-}" == "-h" ]]; then
  echo "用法：$0 [运行目录]"
  echo "优先级：入参 > EZ_ASSISTANT_RUNTIME_HOME > <项目根目录>/.runtime-test"
  exit 0
fi
if (( $# > 1 )) || [[ $# == 1 && -z "$1" ]]; then
  echo "用法：$0 [运行目录]（目录不可为空）" >&2
  exit 2
fi

script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
project_root="$(cd -- "$script_dir/../.." && pwd)"
runtime_home="${1:-${EZ_ASSISTANT_RUNTIME_HOME:-$project_root/.runtime-test}}"
export EZ_ASSISTANT_RUNTIME_HOME
EZ_ASSISTANT_RUNTIME_HOME="$(node -e 'console.log(require("node:path").resolve(process.argv[1]))' -- "$runtime_home")"
cd "$script_dir"
echo "Runtime Home: $EZ_ASSISTANT_RUNTIME_HOME"

# 先完成 Web 嵌入构建，失败时保留正在运行的 Host。
# 后续 cargo run 也必须继承此变量，否则 build.rs 会重建成不带 Web 的开发包。
export EZ_ASSISTANT_WEB_DIST="$PWD/dist"
npm run build:host

# 只停止目标目录登记的 Host；拒绝失效发现文件指向的其他进程，不全局 pkill。
node <<'NODE'
const fs = require("node:fs");
const path = require("node:path");
const { execFileSync } = require("node:child_process");
const { setTimeout: delay } = require("node:timers/promises");

async function stop() {
  const home = process.env.EZ_ASSISTANT_RUNTIME_HOME;
  let discovery;
  try { discovery = JSON.parse(fs.readFileSync(path.join(home, "run/runtime.json"), "utf8")); }
  catch (error) { if (error.code === "ENOENT") return; throw new Error("无法读取目标目录的 Runtime 进程信息。"); }
  const pid = discovery.pid;
  if (!Number.isSafeInteger(pid) || pid <= 1) throw new Error("Runtime PID 无效，未停止任何进程。");
  function alive() {
    try { process.kill(pid, 0); return true; }
    catch (error) { if (error.code === "ESRCH") return false; throw error; }
  }
  if (!alive()) return;
  const command = execFileSync("ps", ["-ww", "-p", String(pid), "-o", "command="], { encoding: "utf8" }).trim();
  // `launch` 生成 detached serve 进程，手动前台启动则没有该参数。仅接受这两种
  // 完整命令形态，继续拒绝同名进程、额外参数或指向其他 Runtime Home 的 PID。
  const expectedCommands = [
    `ez-assistant-runtime serve --runtime-home ${home}`,
    `ez-assistant-runtime serve --detached --runtime-home ${home}`,
  ];
  const matchesTarget = expectedCommands.some(expected =>
    command === expected || command.endsWith(`/${expected}`));
  if (!matchesTarget) {
    throw new Error("登记的 PID 与目标 Runtime Home 不匹配，未停止任何进程。请检查目标目录的运行实例。");
  }
  console.log(`正在停止 Host (${pid})…`);
  process.kill(pid, "SIGINT");
  const deadline = Date.now() + 30_000;
  while (alive()) {
    if (Date.now() >= deadline) throw new Error("Host 在 30 秒内未退出，已停止重启流程。");
    await delay(200);
  }
}
stop().catch(error => { console.error(error.message); process.exitCode = 1; });
NODE

exec node scripts/run-cargo.mjs run -p assistant-runtime-host -- launch --runtime-home "$EZ_ASSISTANT_RUNTIME_HOME"
