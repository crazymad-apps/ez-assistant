#!/bin/bash
set -euo pipefail

if [[ "${1:-}" == "--help" || "${1:-}" == "-h" ]]; then
  echo "用法：$0 [运行目录]"
  echo "优先级：入参 > EZ_ASSISTANT_RUNTIME_HOME > ~/.ez-assistant"
  exit 0
fi
if (( $# > 1 )) || [[ $# == 1 && -z "$1" ]]; then
  echo "用法：$0 [运行目录]（目录不可为空）" >&2
  exit 2
fi

# 相对路径按调用位置解析；切换到脚本目录后仍传递同一个绝对路径。
runtime_home="${1:-${EZ_ASSISTANT_RUNTIME_HOME:-$HOME/.ez-assistant}}"
export EZ_ASSISTANT_RUNTIME_HOME
EZ_ASSISTANT_RUNTIME_HOME="$(node -e 'console.log(require("node:path").resolve(process.argv[1]))' -- "$runtime_home")"
cd "$(dirname "$0")"
echo "Runtime Home: $EZ_ASSISTANT_RUNTIME_HOME"
exec npm run tauri -- dev
