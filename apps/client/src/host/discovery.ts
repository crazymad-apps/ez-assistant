import { constants } from "node:fs";
import { lstat, open, realpath } from "node:fs/promises";
import { isAbsolute, join, parse } from "node:path";
import { homedir } from "node:os";
import { ClientError, object } from "../errors.js";

export interface Discovery { address: string; instance_id: string; access_token: string; pid: number; executable_path?: string; executable_sha256?: string; }
export function missing(error: unknown): boolean { return error instanceof Error && "code" in error && error.code === "ENOENT"; }
export function alive(pid: number): boolean {
  try { process.kill(pid, 0); return true; } catch (error) {
    if (error instanceof Error && "code" in error && error.code === "ESRCH") return false;
    throw new ClientError("process_unknown", "无法确认 Host 进程状态。");
  }
}
/** 按路径分段解析现存 symlink，缺失后缀只做词法归一化；不创建或修复目录。 */
export async function canonicalHome(input?: string): Promise<string> {
  const path = input ?? process.env.EZ_ASSISTANT_RUNTIME_HOME ?? join(homedir(), ".ez-assistant");
  if (!isAbsolute(path) || /[\u0000-\u001f\u007f]/.test(path)) throw new ClientError("usage", "Runtime Home 必须为绝对路径。", 2);
  let current = parse(path).root;
  for (const part of path.slice(current.length).split("/")) {
    if (!part || part === ".") continue;
    current = join(current, part);
    try { current = await realpath(current); } catch (error) {
      if (!missing(error)) throw unsafe();
      try { await lstat(current); throw unsafe(); } catch (error) { if (!missing(error)) throw unsafe(); }
    }
  }
  return current;
}
export function unsafe(): ClientError { return new ClientError("discovery_invalid", "Runtime Home 或发现文件不安全，状态无法核实；请核对文件权限与来源。"); }
export async function privateDirectory(path: string): Promise<boolean> {
  try {
    const stat = await lstat(path);
    if (!stat.isDirectory() || stat.uid !== process.getuid?.() || (stat.mode & 0o077) !== 0) throw unsafe();
    return true;
  } catch (error) { if (missing(error)) return false; throw unsafe(); }
}
export async function readDiscovery(home: string): Promise<Discovery | null> {
  if (!await privateDirectory(home) || !await privateDirectory(join(home, "run"))) return null;
  const path = join(home, "run/runtime.json");
  let file;
  try {
    const before = await lstat(path);
    if (!before.isFile() || before.uid !== process.getuid?.() || (before.mode & 0o077) !== 0 || before.size > 65536) throw unsafe();
    file = await open(path, constants.O_RDONLY | constants.O_NOFOLLOW);
    const opened = await file.stat();
    if (before.ino !== opened.ino || before.dev !== opened.dev || before.mode !== opened.mode || before.uid !== opened.uid || opened.size > 65536) throw unsafe();
    const bytes = Buffer.alloc(65537);
    const { bytesRead } = await file.read(bytes, 0, bytes.length, 0);
    if (bytesRead > 65536) throw unsafe();
    const row = object(JSON.parse(bytes.subarray(0, bytesRead).toString("utf8")));
    if (typeof row.address !== "string" || typeof row.instance_id !== "string" || !/^[\w-]{1,128}$/.test(row.instance_id)
      || typeof row.access_token !== "string" || !/^[\w-]{43}$/.test(row.access_token)
      || !Number.isInteger(row.pid) || Number(row.pid) <= 0 || Number(row.pid) > 0x7fffffff) throw unsafe();
    const url = new URL(row.address);
    if (!["http:", "https:"].includes(url.protocol) || url.hostname !== "127.0.0.1" || url.username || url.password || url.search || url.hash || url.pathname !== "/") throw unsafe();
    if (row.executable_path !== undefined && (typeof row.executable_path !== "string" || !isAbsolute(row.executable_path))) throw unsafe();
    if (row.executable_sha256 !== undefined && (typeof row.executable_sha256 !== "string" || !/^[a-f0-9]{64}$/.test(row.executable_sha256))) throw unsafe();
    return row as unknown as Discovery;
  } catch (error) { if (missing(error)) return null; throw unsafe(); }
  finally { await file?.close(); }
}
export function sameInstance(a: Discovery, b: Discovery): boolean {
  return a.instance_id === b.instance_id && a.access_token === b.access_token && a.pid === b.pid && a.address === b.address;
}
