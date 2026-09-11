import { lstat, readFile } from "node:fs/promises";
import { resolve } from "node:path";
import { userInfo } from "node:os";
import { setTimeout as delay } from "node:timers/promises";
import { ClientError, cancelled } from "../../errors.js";
import { missing, type Discovery } from "../../host/discovery.js";
import { buildInfo, digest } from "../../host/process.js";
import { querySystemd, unitDirectory, type KnownService } from "./query.js";
import { readUnit } from "./status.js";
import { serviceCommand } from "./command.js";
import { conflict, managerArgs, serviceScope } from "./unit.js";

/** 自启未注册时不依赖用户管理器；已有注册或配置冲突不能退化为手动启动。 */
export async function serviceForHome(home: string): Promise<KnownService | null> {
  const observation = await querySystemd(home);
  if (observation.kind === "unsupported") return null;
  if (observation.kind === "known") return observation;
  if (observation.kind === "unavailable") {
    // 系统使用 systemd 不代表当前登录会话提供用户管理器（例如 root SSH）。
    // 只在固定自启注册确实不存在时允许手动管理；不可读、链接或修改过的 unit 仍拒绝。
    const user = userInfo();
    if (!await readUnit(await unitDirectory(), home, user.homedir, user.uid, serviceScope())) return null;
  }
  throw conflict(observation.reason);
}

export async function runningService(home: string, target: Discovery): Promise<KnownService | null> {
  const service = await serviceForHome(home);
  if (!service?.source || service.state.mainPid === 0) return null;
  if (service.state.mainPid !== target.pid || service.source.executable !== target.executable_path) throw conflict("活动服务与当前 Host 的进程或来源不一致，未执行启停。");
  return service;
}

export async function launchService(home: string, expected: KnownService, signal: AbortSignal): Promise<void> {
  if (!expected.source) throw conflict("服务缺少登记来源。");
  await digest(expected.source.executable); await buildInfo(expected.source.executable);
  cancelled(signal);
  let current = await serviceForHome(home);
  if (!current || JSON.stringify(current.source) !== JSON.stringify(expected.source)) throw conflict("服务来源已变化，未改用其他来源启动。");
  // 等待停止 job 完整收敛后才启动；activating 已有启动 job，不重复提交。
  if (current.state.activeState === "deactivating") {
    await waitServiceStopped(home, current, 30, signal);
    current = await serviceForHome(home);
    if (!current || JSON.stringify(current.source) !== JSON.stringify(expected.source)) throw conflict("等待期间服务来源已变化。");
  }
  if (["activating", "active"].includes(current.state.activeState)) return;
  await serviceCommand("systemctl", [...managerArgs(expected.scope), "--no-ask-password", "--no-block", "start", expected.unit]);
}

export async function verifyServiceInstance(home: string, expected: KnownService, target: Discovery): Promise<void> {
  const current = await serviceForHome(home);
  if (!current || JSON.stringify(current.source) !== JSON.stringify(expected.source) || current.state.mainPid !== target.pid
    || target.executable_path !== expected.source?.executable || current.state.activeState !== "active") throw conflict("就绪 Host 与预期服务来源不一致，保留当前实例，请查询状态。");
}

export async function waitServiceStopped(home: string, expected: KnownService, timeout: number, signal: AbortSignal): Promise<void> {
  const deadline = Date.now() + timeout * 1000;
  while (Date.now() < deadline) {
    cancelled(signal);
    const current = await serviceForHome(home);
    if (!current || JSON.stringify(current.source) !== JSON.stringify(expected.source)
      || current.state.mainPid !== 0 && current.state.mainPid !== expected.state.mainPid) throw conflict("停止期间服务来源或实例已变化；未补发停止命令。");
    if (current.state.mainPid === 0 && ["inactive", "failed"].includes(current.state.activeState) && await emptyServiceGroup(current.state.controlGroup)) return;
    await delay(250, undefined, { signal });
  }
  throw new ClientError("timeout", "Host 已请求停止，但服务 job 或子进程尚未收敛；未强杀或重新启动。");
}

async function emptyServiceGroup(group: string): Promise<boolean> {
  if (!group) return true;
  const root = "/sys/fs/cgroup", path = resolve(root, `.${group}`);
  if (!group.startsWith("/") || !path.startsWith(`${root}/`)) throw conflict("服务 cgroup 路径无法核实。");
  try {
    const events = await readFile(`${path}/cgroup.events`, "utf8");
    if (!/^populated [01]$/m.test(events)) throw conflict("无法核实服务残留进程。");
    return /^populated 0$/m.test(events);
  } catch (error) {
    if (missing(error)) {
      try { await lstat(path); } catch (directoryError) { if (missing(directoryError)) return true; throw directoryError; }
      throw conflict("当前 cgroup 不提供进程收敛状态，无法确认服务已完整停止。");
    }
    throw error;
  }
}
