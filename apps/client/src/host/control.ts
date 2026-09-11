import { lstat } from "node:fs/promises";
import { join } from "node:path";
import { setTimeout as delay } from "node:timers/promises";
import { checkCompatibility, currentCompatibility, type HostAccessCommand, type HostAccessStatus, type RuntimeHostCapabilities, type RuntimeHostHealth } from "@ez-assistant/protocol/node";
import { ClientError, cancelled, errorFromHost, object } from "../errors.js";
import { alive, missing, privateDirectory, readDiscovery, sameInstance, type Discovery } from "./discovery.js";
import { accessCommand, accessStatus, capabilities, health, shutdown } from "./http.js";
import { portOccupied } from "./port.js";
import { buildInfo, bundledSource, localProcess, verifySource } from "./process.js";
import { launchService, runningService, serviceForHome, verifyServiceInstance, waitServiceStopped } from "../platform/systemd/lifecycle.js";

export interface Target { discovery: Discovery; capabilities: RuntimeHostCapabilities; health: RuntimeHostHealth; }
export class HostControl {
  constructor(readonly home: string, readonly signal: AbortSignal, readonly report: (message: string) => void = () => {}) {}
  async helper(operation: string, input?: unknown): Promise<unknown> {
    const source = bundledSource(); await buildInfo(source);
    const result = await localProcess(source, ["access", operation, "--runtime-home", this.home], input);
    if (result.code !== 0) throw errorFromHost(result.value);
    return result.value;
  }
  async locked(): Promise<boolean> {
    if (!await privateDirectory(this.home) || !await privateDirectory(join(this.home, "run"))) return false;
    try { await lstat(join(this.home, "run/runtime.lock")); } catch (error) { if (missing(error)) return false; throw error; }
    const row = object(await this.helper("probe"));
    if (typeof row.lock_held !== "boolean") throw new ClientError("invalid_response", "无法核实 Host 实例锁。");
    return row.lock_held;
  }
  async target(): Promise<Target | null> {
    cancelled(this.signal);
    const discovery = await readDiscovery(this.home);
    if (!discovery || !alive(discovery.pid)) return null;
    const caps = await capabilities(discovery, this.signal);
    const issue = checkCompatibility(currentCompatibility(), { version: caps.runtime_version, min_compatible_version: caps.min_compatible_version });
    if (issue) {
      const ours = currentCompatibility();
      throw new ClientError("incompatible", `软件版本不兼容（${issue.code}）。Client ${ours.version}，最低 Host ${ours.min_compatible_version}；Host ${issue.host?.version ?? "声明无效"}，最低 Client ${issue.host?.min_compatible_version ?? "未知"}。`);
    }
    if (!Array.isArray(caps.features) || !caps.features.includes("host_access") || !caps.features.includes("startup_diagnostics")) throw new ClientError("incompatible", "Host 缺少本机管理所需能力，请使用兼容版本。");
    return { discovery, capabilities: caps, health: await health(discovery, this.signal) };
  }
  async status(): Promise<Target | null> {
    const target = await this.target();
    if (!target && await this.locked()) throw new ClientError("busy", "实例锁被占用，但 Host 尚未发布可验证的地址；当前状态待查询。");
    return target;
  }
  async waitReady(timeout: number, expected?: Discovery): Promise<Target> {
    const deadline = Date.now() + timeout * 1000; let interval = 250; let last = "";
    while (Date.now() < deadline) {
      cancelled(this.signal);
      const target = await this.target();
      if (target) {
        if (expected && !sameInstance(target.discovery, expected)) throw changed();
        if (target.health.status === "unavailable") throw new ClientError("startup_failed", `Host 初始化失败：${target.health.error ?? "未知原因"}；数据库最低要求 ${target.health.min_compatible_host_version ?? "未确认"}，保留现有诊断。`);
        if (target.health.status === "ready") return target;
        const stage = target.health.stage ?? "initializing";
        if (stage !== last) { this.report(`Host 初始化阶段：${stage}`); last = stage; }
        expected ??= target.discovery;
      }
      await delay(interval, undefined, { signal: this.signal }); interval = Math.min(1000, interval * 2);
    }
    throw new ClientError("timeout", "Host 未能在限定时间内就绪，结果待查询；请执行 status，不要重复启动。");
  }
  async start(timeout = 60): Promise<{ target: Target; reused: boolean }> {
    let current = await this.target();
    if (current) return { target: await this.waitReady(timeout, current.discovery), reused: true };
    const deadline = Date.now() + timeout * 1000;
    // 锁可能由启动中的 Host 或短时配置持有，等待已有操作；不创建另一套锁。
    while (await this.locked()) {
      cancelled(this.signal);
      current = await this.target();
      if (current) return { target: await this.waitReady(Math.max(1, (deadline - Date.now()) / 1000), current.discovery), reused: true };
      if (Date.now() >= deadline) throw new ClientError("timeout", "实例锁持续被占用，启动结果待查询。");
      await delay(250, undefined, { signal: this.signal });
    }
    const service = await serviceForHome(this.home);
    if (service?.source && (service.state.fileState === "enabled" || ["active", "activating", "deactivating"].includes(service.state.activeState))) {
      this.report(`启动服务来源：${service.source.executable}`);
      await launchService(this.home, service, this.signal);
      const target = await this.waitReady(Math.max(1, (deadline - Date.now()) / 1000));
      await verifyServiceInstance(this.home, service, target.discovery);
      return { target, reused: false };
    }
    const source = bundledSource(); await buildInfo(source); cancelled(this.signal);
    const configuration = accessStatus({ ...object(await this.helper("read")), listener_state: "closed", restart_required: false, error: null }).configuration;
    if (await portOccupied(configuration.port, this.signal)) {
      const winner = await this.target();
      if (winner) return { target: await this.waitReady(Math.max(1, (deadline - Date.now()) / 1000), winner.discovery), reused: true };
      // Host 可能已绑定端口、仍在发布 discovery；由同一内核锁确认启动竞争。
      if (await this.locked()) return { target: await this.waitReady(Math.max(1, (deadline - Date.now()) / 1000)), reused: true };
      throw new ClientError("port_in_use", `端口 ${configuration.port} 已被占用；未启动 Host，也未更换端口。`);
    }
    cancelled(this.signal);
    this.report(`启动来源：${source}`);
    const result = await localProcess(source, ["launch", "--runtime-home", this.home], undefined, 3000);
    if (result.code !== 0) throw new ClientError("start_failed", "Host 启动器失败，请检查端口、配置与目录权限。");
    return { target: await this.waitReady(Math.max(1, (deadline - Date.now()) / 1000)), reused: false };
  }
  async waitStopped(original: Discovery, timeout: number): Promise<void> {
    const deadline = Date.now() + timeout * 1000;
    while (Date.now() < deadline) {
      cancelled(this.signal);
      const current = await readDiscovery(this.home);
      if (current && !sameInstance(current, original)) throw changed();
      if (!alive(original.pid) && !await this.locked()) return;
      await delay(250, undefined, { signal: this.signal });
    }
    throw new ClientError("timeout", "原 Host 尚未确认停止，结果待查询；未强制结束进程。");
  }
  async stop(timeout = 30): Promise<boolean> {
    const target = await this.status();
    if (!target) {
      const service = await serviceForHome(this.home);
      if (service?.source) await waitServiceStopped(this.home, service, timeout, this.signal);
      return false;
    }
    const service = await runningService(this.home, target.discovery);
    const deadline = Date.now() + timeout * 1000;
    await shutdown(target.discovery, this.signal);
    await this.waitStopped(target.discovery, timeout);
    if (service) await waitServiceStopped(this.home, service, Math.max(0.1, (deadline - Date.now()) / 1000), this.signal);
    return true;
  }
  async restart(timeout?: number): Promise<Target> {
    const target = await this.status();
    if (!target) throw new ClientError("stopped", "Host 尚未启动，请使用 start。");
    const original = target.discovery;
    const version = { version: target.capabilities.runtime_version, min_compatible_version: target.capabilities.min_compatible_version };
    const source = await verifySource(original.executable_path, original.executable_sha256, version);
    const service = await runningService(this.home, original);
    cancelled(this.signal); this.report(`重启来源：${source}`);
    const stopDeadline = Date.now() + (timeout ?? 30) * 1000;
    await shutdown(original, this.signal); await this.waitStopped(original, timeout ?? 30);
    if (service) await waitServiceStopped(this.home, service, Math.max(0.1, (stopDeadline - Date.now()) / 1000), this.signal);
    this.report("原 Host 已停止");
    try {
      await verifySource(source, original.executable_sha256, version); cancelled(this.signal);
      if (await this.target() || await this.locked()) throw changed();
      if (service) await launchService(this.home, service, this.signal);
      else {
        const result = await localProcess(source, ["launch", "--runtime-home", this.home], undefined, 3000);
        if (result.code !== 0) throw new ClientError("start_failed", "原来源启动器失败。");
      }
      const current = await this.waitReady(timeout ?? 60);
      if (current.discovery.instance_id === original.instance_id || current.discovery.executable_path !== source || current.discovery.executable_sha256 !== original.executable_sha256) throw changed();
      if (service) await verifyServiceInstance(this.home, service, current.discovery);
      return current;
    } catch (error) {
      if (error instanceof ClientError) throw new ClientError(error.code, `原 Host 已停止，本次重启未完成。${error.message}`);
      throw error;
    }
  }
  async configuration(): Promise<ConfigurationSession> {
    const target = await this.status();
    if (target && target.health.status !== "ready") throw new ClientError("not_ready", `Host ${target.health.status}，不能当作离线编辑；请等待或显式停止。`);
    const session = new ConfigurationSession(this, target?.discovery ?? null);
    await session.read(); return session;
  }
}
export class ConfigurationSession {
  status!: HostAccessStatus;
  constructor(readonly host: HostControl, readonly target: Discovery | null) {}
  async read(): Promise<HostAccessStatus> {
    if (this.target) {
      const current = await this.host.target();
      if (!current || !sameInstance(current.discovery, this.target) || current.health.status !== "ready") throw changed();
      this.status = await accessCommand(this.target, { type: "get_status" });
    } else {
      if (await this.host.target() || await this.host.locked()) throw new ClientError("busy", "Host 已启动或正在提交配置，请重新读取。");
      this.status = accessStatus({ ...object(await this.host.helper("read")), listener_state: "closed", restart_required: false, error: null });
    }
    return this.status;
  }
  async save(command: Exclude<HostAccessCommand, { type: "get_status" }>): Promise<HostAccessStatus> {
    cancelled(this.host.signal);
    let saved: HostAccessStatus;
    if (this.target) {
      const current = await this.host.target();
      if (!current || !sameInstance(current.discovery, this.target) || current.health.status !== "ready") throw changed();
      saved = await accessCommand(this.target, command);
    } else saved = accessStatus({ ...object(await this.host.helper(command.type === "configure" ? "configure" : "set-password", command.payload)), listener_state: "closed", restart_required: false, error: null });
    // 提交后回读不接受取消；结果必须明确，不能把取消解释成写入回滚。
    try {
      const latest = this.target ? await accessCommand(this.target, { type: "get_status" }) : accessStatus({ ...object(await this.host.helper("read")), listener_state: "closed", restart_required: false, error: null });
      if (latest.revision !== saved.revision) throw new Error();
      this.status = latest; return latest;
    } catch { throw new ClientError("committed_unknown", "已提交，但当前生效状态未核实，请重新读取；不要重复提交。"); }
  }
}
function changed(): ClientError { return new ClientError("instance_changed", "操作期间 Host 实例已变化；保留当前实例，请重新查询。"); }
