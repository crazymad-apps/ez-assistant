import { spawn } from "node:child_process";
import { createHash } from "node:crypto";
import { open, readFile, realpath, stat } from "node:fs/promises";
import { dirname, isAbsolute, join, resolve } from "node:path";
import { checkCompatibility, currentCompatibility, type ClientCompatibility } from "@ez-assistant/protocol/node";
import { ClientError, object } from "../errors.js";

import { npmSource } from "../distribution/npm-source.js";

export function bundledSource(): string {
  const source = process.env.EZ_ASSISTANT_RUNTIME_EXECUTABLE ?? npmSource() ?? resolve(import.meta.dirname, "../../host/ez-assistant-runtime");
  if (!isAbsolute(source)) throw new ClientError("usage", "Host 可执行文件覆盖路径必须为绝对路径。", 2);
  return source;
}
/** 有界短命令。超时不强杀保存进程；取消也不能撤销已提交的原子写入。 */
export function localProcess(source: string, args: string[], input?: unknown, timeout = 15000): Promise<{ code: number | null; value: unknown }> {
  return new Promise((accept, reject) => {
    const child = spawn(source, args, { stdio: ["pipe", "pipe", "ignore"] });
    const chunks: Buffer[] = []; let size = 0; let settled = false;
    const finish = (error?: ClientError, result?: { code: number | null; value: unknown }) => {
      if (settled) return; settled = true; clearTimeout(timer);
      if (error) reject(error); else accept(result!);
    };
    const timer = setTimeout(() => {
      child.stdin.destroy(); child.stdout.destroy(); child.unref();
      finish(new ClientError("result_unknown", "Host 短命令等待超时，结果待查询；请重新读取，勿重复提交。"));
    }, timeout);
    child.on("error", () => finish(new ClientError("source_unavailable", "Host 启动来源不可用，请检查安装或显式开发路径。")));
    child.stdout.on("data", (chunk: Buffer) => {
      size += chunk.length;
      if (size > 65536) {
        child.stdout.destroy(); child.unref(); finish(new ClientError("invalid_response", "Host 响应超过允许范围，结果待查询。"));
      } else chunks.push(chunk);
    });
    child.stdin.on("error", () => { /* 子进程可能在读取正文前拒绝；以退出结果为准，避免输出密码。 */ });
    child.on("close", (code) => {
      if (settled) return;
      try {
        const output = Buffer.concat(chunks).toString("utf8").trim();
        finish(undefined, { code, value: output ? JSON.parse(output) as unknown : null });
      } catch { finish(new ClientError("invalid_response", "Host 未返回有效结果，请查询当前状态。")); }
    });
    child.stdin.end(input === undefined ? undefined : JSON.stringify(input));
  });
}
export async function buildInfo(source: string): Promise<ClientCompatibility> {
  if (!process.env.EZ_ASSISTANT_RUNTIME_EXECUTABLE && npmSource() === source) {
    const row = object(JSON.parse(await readFile(join(dirname(source), "manifest.json"), "utf8")) as unknown);
    const expected = currentCompatibility();
    if (row.version !== expected.version || row.min_compatible_version !== expected.min_compatible_version
      || row.platform !== process.platform || row.arch !== process.arch || typeof row.sha256 !== "string"
      || !/^[a-f0-9]{64}$/.test(row.sha256) || await digest(source) !== row.sha256) {
      throw new ClientError("package_invalid", "平台包内容与清单不一致，未执行 Host。");
    }
  }
  const result = await localProcess(source, ["--build-info-json"], undefined, 3000);
  const row = object(result.value);
  if (result.code !== 0 || checkCompatibility(currentCompatibility(), row)) throw new ClientError("incompatible", "随包 Host 与 Client 软件版本不兼容，无法调用本机管理入口。");
  return row as unknown as ClientCompatibility;
}
export async function digest(source: string): Promise<string> {
  try {
    if (!isAbsolute(source) || await realpath(source) !== source) throw new Error();
    const file = await open(source, "r");
    try {
      const before = await file.stat();
      if (!before.isFile() || !(before.mode & 0o111)) throw new Error();
      const hash = createHash("sha256"); const buffer = Buffer.alloc(65536);
      for (;;) { const { bytesRead } = await file.read(buffer, 0, buffer.length, null); if (!bytesRead) break; hash.update(buffer.subarray(0, bytesRead)); }
      const after = await stat(source);
      if (before.dev !== after.dev || before.ino !== after.ino || before.size !== after.size || before.mtimeMs !== after.mtimeMs) throw new Error();
      return hash.digest("hex");
    } finally { await file.close(); }
  } catch { throw new ClientError("source_unavailable", "原 Host 来源缺失、不可执行或已改变；未改用其他来源。"); }
}
export async function verifySource(source: string | undefined, expectedHash: string | undefined, expected: ClientCompatibility): Promise<string> {
  if (!source || !expectedHash || await digest(source) !== expectedHash) throw new ClientError("source_unavailable", "原 Host 来源缺失或摘要已变化，无法重启。");
  const actual = await buildInfo(source);
  if (actual.version !== expected.version || actual.min_compatible_version !== expected.min_compatible_version || await digest(source) !== expectedHash) throw new ClientError("source_unavailable", "原 Host 来源版本或内容已变化，无法重启。");
  return source;
}
