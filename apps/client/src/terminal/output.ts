import * as p from "@clack/prompts";
import pc from "picocolors";
import ora from "ora";
import { stripVTControlCharacters } from "node:util";
import type { Target } from "../host/control.js";
export function safe(value: string): string { return stripVTControlCharacters(value).replace(/[\u0000-\u0008\u000b-\u001f\u007f-\u009f]/g, ""); }
export function line(value: string): void { console.log(safe(value)); }
export function targetHome(home: string): void { line(`目标目录  ${home}`); }
export function showTarget(target: Target | null): void {
  if (!target) { line("Host      未启动"); return; }
  const labels = { ready: "业务就绪", starting: "初始化中", unavailable: "初始化失败" };
  line(`Host      ${labels[target.health.status]}`);
  line(`软件版本  ${target.capabilities.runtime_version} · 最低兼容 ${target.capabilities.min_compatible_version}`);
  line(`当前地址  ${target.discovery.address}`);
  if (target.health.stage) line(`当前阶段  ${target.health.stage}`);
  if (target.health.error) line(`失败原因  ${target.health.error} · 数据库最低要求 ${target.health.min_compatible_host_version ?? "未知"}`);
  if (target.discovery.executable_path) line(`Host 来源 ${target.discovery.executable_path}`);
}
export async function working<T>(message: string, action: () => Promise<T>): Promise<T> {
  if (!process.stdout.isTTY || process.env.TERM === "dumb") { line(message); return action(); }
  const spinner = ora({ text: message, stream: process.stdout, discardStdin: false, color: pc.isColorSupported ? "cyan" : false }).start();
  try { return await action(); } finally { spinner.stop(); }
}
export function note(message: string, title: string): void { p.note(safe(message), safe(title)); }
