import { constants } from "node:fs";
import { lstat, open, realpath } from "node:fs/promises";
import { join, resolve } from "node:path";
import { conflict, parseUnit, unitName, type UnitSource, type ServiceScope } from "./unit.js";
import { missing } from "../../host/discovery.js";

export interface UnitState {
  fileState: "not-found" | "disabled" | "enabled";
  activeState: "inactive" | "active" | "activating" | "deactivating" | "failed";
  subState: string;
  mainPid: number;
  result: string;
  fragmentPath: string;
  execStart: string;
  controlGroup: string;
}
export type AutostartState = "unconfigured" | "disabled" | "enabled" | "incomplete";

/** 只投影 OS 已核实的事实；enabled 与业务 Ready 是两种状态。 */
export function autostartState(state: UnitState, linger: boolean | null, source: UnitSource | null, scope: ServiceScope = "user"): AutostartState {
  if (!source) {
    if (state.fileState !== "not-found" || state.mainPid !== 0 || state.activeState !== "inactive") throw conflict("服务注册与实际运行状态不一致。");
    return "unconfigured";
  }
  if (state.fileState === "disabled") return "disabled";
  if (state.fileState === "enabled" && (scope === "system" || linger === true)) return "enabled";
  return "incomplete";
}

export function parseProperties(output: string): UnitState {
  const properties = new Map<string, string>();
  for (const line of output.trimEnd().split("\n")) {
    const equals = line.indexOf("=");
    if (equals < 1) throw conflict("systemd 状态响应无效。");
    const key = line.slice(0, equals);
    if (properties.has(key)) throw conflict("systemd 状态包含重复字段。");
    properties.set(key, line.slice(equals + 1));
  }
  const required = (name: string): string => {
    const value = properties.get(name);
    if (value === undefined) throw conflict(`systemd 未提供 ${name}，当前状态未知。`);
    return value;
  };
  if (required("DropInPaths") !== "" || required("NeedDaemonReload") !== "no") throw conflict("服务含覆盖配置或尚未重新加载，不能按现有文件管理。");
  const load = required("LoadState"), rawFile = required("UnitFileState"), active = required("ActiveState"), pid = required("MainPID");
  const file = load === "not-found" && rawFile === "" ? "not-found" : rawFile;
  if (!["loaded", "not-found"].includes(load) || !["not-found", "disabled", "enabled"].includes(file)
    || !["inactive", "active", "activating", "deactivating", "failed"].includes(active)
    || !/^\d+$/.test(pid) || !Number.isSafeInteger(Number(pid)) || Number(pid) > 0x7fffffff) throw conflict("systemd 注册或进程状态无法核实。");
  const state: UnitState = { fileState: file as UnitState["fileState"], activeState: active as UnitState["activeState"],
    mainPid: Number(pid), fragmentPath: required("FragmentPath"), execStart: load === "not-found" ? properties.get("ExecStart") ?? "" : required("ExecStart"), subState: required("SubState"), result: required("Result"), controlGroup: required("ControlGroup") };
  if (load === "not-found" && (state.fragmentPath || state.execStart || state.mainPid || state.activeState !== "inactive")) throw conflict("未注册服务返回了运行信息。");
  if (load === "loaded" && (!state.fragmentPath || !state.execStart || file === "not-found")) throw conflict("服务缺少可核实的注册来源。");
  return state;
}

/** 读取固定名称的原生 unit，不创建配置目录，也不修复任何权限。 */
export async function readUnit(directory: string, home: string, userHome: string, uid: number, scope: ServiceScope = "user"): Promise<{ text: string; source: UnitSource } | null> {
  const path = join(directory, unitName(home));
  let handle;
  try {
    const before = await lstat(path);
    if (!before.isFile() || before.uid !== uid || (before.mode & 0o022) || before.size > 65536 || await realpath(path) !== resolve(path)) throw conflict("服务文件类型、归属、路径或权限不安全。");
    // 检查目录链的写权限；拒绝另一用户能够替换的祖先，根目录只允许 root 所有。
    for (let current = directory; ; current = resolve(current, "..")) {
      const metadata = await lstat(current);
      // Linux /tmp 是 root 所有的 sticky 目录；其他用户不能替换我们已核实归属的子项。
      const protectedTemporaryRoot = metadata.uid === 0 && (metadata.mode & 0o1000) !== 0;
      if (!metadata.isDirectory() || ![uid, 0].includes(metadata.uid) || ((metadata.mode & 0o022) && !protectedTemporaryRoot)) throw conflict("服务配置目录可以被其他用户改写。");
      if (current === resolve(current, "..")) break;
    }
    handle = await open(path, constants.O_RDONLY | constants.O_NOFOLLOW);
    const opened = await handle.stat();
    if (opened.ino !== before.ino || opened.dev !== before.dev || opened.mode !== before.mode || opened.uid !== before.uid || opened.size > 65536) throw conflict("服务文件在读取时发生变化。");
    const bytes = Buffer.alloc(65537);
    const { bytesRead } = await handle.read(bytes, 0, bytes.length, 0);
    if (bytesRead > 65536) throw conflict("服务文件超过允许范围。");
    const text = bytes.subarray(0, bytesRead).toString("utf8");
    return { text, source: parseUnit(text, home, userHome, scope) };
  } catch (error) {
    if (missing(error)) return null;
    throw error;
  } finally { await handle?.close(); }
}
