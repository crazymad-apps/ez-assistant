import { constants } from "node:fs";
import { lstat, mkdir, open } from "node:fs/promises";
import { join } from "node:path";
import { userInfo } from "node:os";
import { fileURLToPath } from "node:url";
import { spawn } from "node:child_process";
import type { ClientCompatibility } from "@ez-assistant/protocol/node";
import { ClientError, cancelled } from "../../errors.js";
import { canonicalHome, privateDirectory } from "../../host/discovery.js";
import { buildInfo, bundledSource, digest } from "../../host/process.js";
import { querySystemd, serviceLines, type KnownService } from "./query.js";
import { renderUnit, conflict, serviceScope, type UnitSource } from "./unit.js";
import { serviceCommand } from "./command.js";

export interface ServiceDraft {
  home: string;
  expected: KnownService;
  enabled: boolean;
  source: UnitSource | null;
  version: ClientCompatibility | null;
  sha256: string | null;
}

/** 预览只读；现有注册默认沿用，切换到随包来源必须是独立用户意图。 */
export async function prepareService(home: string, enabled: boolean, replaceSource = false): Promise<ServiceDraft> {
  const expected = await querySystemd(home);
  if (expected.kind !== "known") throw conflict(expected.reason);
  await serviceCommand("flock", ["--version"]);
  await lockDirectory();
  let source = expected.source;
  if (replaceSource || (enabled && !source)) source = { executable: bundledSource(), runtimeHome: home, userHome: userInfo().homedir };
  if (replaceSource && (expected.state.mainPid !== 0 || !["inactive", "failed"].includes(expected.state.activeState))) throw conflict("请先显式停止服务，再切换注册来源。");
  let version: ClientCompatibility | null = null, sha256: string | null = null;
  // 关闭不依赖 Host 可执行文件可用或应用协议兼容。
  if (source && (enabled || replaceSource)) {
    renderUnit(source, expected.scope); sha256 = await digest(source.executable); version = await buildInfo(source.executable);
  }
  return { home, expected, enabled, source, version, sha256 };
}

export function servicePreview(draft: ServiceDraft): string[] {
  return [...serviceLines(draft.expected), `保存后自启  ${draft.enabled ? "开启" : "关闭"}`,
    ...(draft.source ? [`保存后来源  ${draft.source.executable}`] : []),
    ...(draft.version ? [`来源版本  ${draft.version.version}`] : []),
    ...(draft.expected.scope === "user" && draft.enabled && draft.expected.linger !== true ? [`系统前提  loginctl enable-linger ${draft.expected.uid}`] : []),
    draft.expected.scope === "system" ? "本次保存不启动、停止或重启当前 Host。" : "本次保存不启动、停止或重启当前 Host；关闭自启不关闭用户 linger。"];
}

async function lockDirectory(create = false): Promise<string> {
  if (serviceScope() === "system") {
    const parent = await lstat("/run");
    if (!parent.isDirectory() || parent.uid !== 0 || (parent.mode & 0o022)) throw conflict("系统运行目录不安全。");
    const directory = "/run/ez-assistant-client";
    if (create) await mkdir(directory, { mode: 0o700, recursive: true });
    if (await canonicalHome(directory) !== directory) throw conflict("系统提交锁目录不能包含路径别名。");
    await privateDirectory(directory); // 缺失可用于只读预览；保存阶段创建后再校验。
    return directory;
  }
  const directory = process.env.XDG_RUNTIME_DIR || `/run/user/${userInfo().uid}`;
  if (await canonicalHome(directory) !== directory || !await privateDirectory(directory)) throw conflict("当前用户的私有运行目录不可用，无法取得自启提交锁。");
  return directory;
}

/** flock 只覆盖短提交，已提交后等待子进程回执，取消不重放或撤销保存。 */
export async function saveService(draft: ServiceDraft, signal: AbortSignal): Promise<KnownService> {
  cancelled(signal);
  const path = join(await lockDirectory(true), `${draft.expected.unit}.lock`);
  const file = await open(path, constants.O_RDWR | constants.O_CREAT | constants.O_NOFOLLOW, 0o600);
  try {
    const stat = await file.stat(), entry = await lstat(path);
    if (!stat.isFile() || stat.uid !== process.getuid?.() || (stat.mode & 0o077) || stat.nlink !== 1 || entry.ino !== stat.ino || entry.dev !== stat.dev) throw conflict("自启提交锁的类型、权限或归属异常。");
    cancelled(signal);
    const development = import.meta.url.endsWith(".ts");
    const worker = fileURLToPath(new URL(development ? "./commit-worker.ts" : "./commit-worker.js", import.meta.url));
    const result = await new Promise<{ code: number | null; output: string }>((accept, reject) => {
      // 通过已安全打开的 fd 加锁，避免 flock 再按可被替换的路径打开文件。
      const child = spawn("flock", ["--exclusive", "--nonblock", "--conflict-exit-code", "75", "--no-fork", "/proc/self/fd/3", process.execPath,
        ...(development ? ["--import", "tsx"] : []), worker], { detached: true, stdio: ["pipe", "pipe", "ignore", file.fd] });
      const chunks: Buffer[] = []; let size = 0;
      child.on("error", () => reject(new ClientError("service_unavailable", "无法启动自启提交程序。")));
      child.stdin!.on("error", () => { /* flock 竞争拒绝时可能不读取输入，以退出码为准。 */ });
      child.stdout!.on("data", (chunk: Buffer) => { size += chunk.length; if (size <= 65536) chunks.push(chunk); });
      child.on("close", (code) => accept({ code, output: size <= 65536 ? Buffer.concat(chunks).toString("utf8") : "" }));
      child.stdin!.end(JSON.stringify(draft));
    });
    if (result.code === 75) throw conflict("另一客户端正在提交启动设置，请重新读取后再保存。");
    if (result.code !== 0) {
      const actual = await querySystemd(draft.home);
      throw new ClientError("service_partial", `${result.output.trim() || "启动设置提交未确认成功。"}\n${serviceLines(actual).join("\n")}\n已完成的系统操作不会自动回滚，请重新读取。`);
    }
    const actual = await querySystemd(draft.home);
    if (actual.kind !== "known" || actual.autostart !== (draft.enabled ? "enabled" : draft.source ? "disabled" : "unconfigured") || JSON.stringify(actual.source) !== JSON.stringify(draft.source)) {
      throw new ClientError("service_partial", `提交后状态未核实，请重新读取。\n${serviceLines(actual).join("\n")}`);
    }
    return actual;
  } finally { await file.close(); }
}
