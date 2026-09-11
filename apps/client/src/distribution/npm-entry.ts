#!/usr/bin/env node
// engines 在部分 npm 设置下仅警告；入口必须在加载 CLI 依赖前拒绝不支持的 Node。
const [major, minor] = process.versions.node.split(".").map(Number);
// Commander 15 要求 Node 22.12+；支持已核定的 22/24 LTS 系列，不自动放行未来主版本。
if (!((major === 22 && minor !== undefined && minor >= 12) || major === 24)) {
  process.stderr.write(`Client 需要 Node 22.12+（22.x）或 24.x，当前 ${process.versions.node}。\n`);
  process.exitCode = 1;
} else {
  await import("../cli.js");
}
export {};
