import { lstat } from "node:fs/promises";
import { spawn } from "node:child_process";
import { userInfo } from "node:os";
import { isAbsolute, join, resolve } from "node:path";
import { canonicalHome, missing } from "../../host/discovery.js";
import { ClientError } from "../../errors.js";
import { unitName, serviceScope, managerArgs, type ServiceScope, type UnitSource } from "./unit.js";
import { autostartState, parseProperties, readUnit, type AutostartState, type UnitState } from "./status.js";

export type ServiceObservation =
  | { kind: "unsupported" | "unavailable" | "conflict"; reason: string }
  | { kind: "known"; scope: ServiceScope; unit: string; uid: number; autostart: AutostartState; linger: boolean | null; state: UnitState; source: UnitSource | null };

export type KnownService = Extract<ServiceObservation, { kind: "known" }>;
export async function unitDirectory(scope: ServiceScope = serviceScope()): Promise<string> {
  if (scope === "system") return "/etc/systemd/system";
  const configHome = process.env.XDG_CONFIG_HOME || join(userInfo().homedir, ".config");
  if (!isAbsolute(configHome) || await canonicalHome(configHome) !== resolve(configHome)) throw new ClientError("service_conflict", "systemd 配置目录必须为无路径别名的绝对路径。");
  return join(configHome, "systemd/user");
}

/** 只读的系统状态入口，与 Host 应用兼容、业务就绪和 Client 安装路径独立。 */
export async function querySystemd(home: string): Promise<ServiceObservation> {
  if (process.platform !== "linux") return { kind: "unsupported", reason: "本平台暂不支持开机自启；手动 Host 管理可用。" };
  try {
    const user = userInfo();
    const scope = serviceScope(), directory = await unitDirectory(scope), unit = unitName(home);
    // 固定名称在另一管理范围存在时拒绝隐式迁移，避免同一 Home 被重复登记。
    const other = join(await unitDirectory(scope === "system" ? "user" : "system"), unit);
    try { await lstat(other); throw new ClientError("service_conflict", `另一 systemd 范围已有同一 Host 注册：${other}；请先显式处理原服务。`); }
    catch (error) { if (!missing(error)) throw error; }
    const properties = ["LoadState", "UnitFileState", "ActiveState", "SubState", "MainPID", "ExecStart", "FragmentPath", "Result", "DropInPaths", "NeedDaemonReload", "ControlGroup"];
    const shown = await query("systemctl", [...managerArgs(scope), "--no-pager", "show", unit, `--property=${properties.join(",")}`]);
    // not-found 也应返回完整可校验属性；连接失败的 stderr 不能当作未配置。
    if (shown.code !== 0 && !shown.output.includes("LoadState=not-found\n")) throw new ClientError("service_unavailable", "systemd 管理器不可用，请检查系统服务；用户级服务还需检查会话与用户总线。");
    const state = parseProperties(shown.output);
    const registration = await readUnit(directory, home, user.homedir, user.uid, scope);
    const reread = await query("systemctl", [...managerArgs(scope), "--no-pager", "show", unit, `--property=${properties.join(",")}`]);
    if (reread.code !== shown.code || JSON.stringify(parseProperties(reread.output)) !== JSON.stringify(state)) throw new ClientError("service_conflict", "读取期间服务状态已变化，请重新查询。");
    if (registration) {
      if (state.fragmentPath !== join(directory, unit)) throw new ClientError("service_conflict", "systemd 实际加载的服务来自其他目录。");
      // 完整文件模板、无 drop-in 和 NeedDaemonReload=no 共同约束实际加载配置。
      // ExecStart 只作为诊断保留；不猜测 systemctl 的非 JSON argv 展示格式。
    }
    let linger: boolean | null = null;
    try {
      if (scope === "user") {
      const login = await query("loginctl", ["show-user", String(user.uid), "--property=Linger", "--value"]);
      if (login.code === 0 && ["yes", "no"].includes(login.output.trim())) linger = login.output.trim() === "yes";
      }
    } catch { /* 单独保留 linger 未知，不抹除已读到的 unit 状态。 */ }
    return { kind: "known", scope, uid: user.uid, unit, state, source: registration?.source ?? null, linger, autostart: autostartState(state, linger, registration?.source ?? null, scope) };
  } catch (error) {
    return { kind: error instanceof ClientError && error.code === "service_conflict" ? "conflict" : "unavailable", reason: error instanceof ClientError ? error.message : "无法读取 systemd 状态；未改动服务或退化为默认关闭。" };
  }
}

export function serviceLines(observation: ServiceObservation): string[] {
  if (observation.kind !== "known") return [`开机自启  ${observation.reason}`];
  const names = { unconfigured: "未配置（默认关闭）", disabled: "已关闭", enabled: "已开启", incomplete: "未完成／状态未知" };
  return [`服务范围  ${observation.scope === "system" ? "系统级" : "用户级"}`, `服务名称  ${observation.unit}`, `开机自启  ${names[observation.autostart]}`, `系统服务  ${observation.state.activeState} / ${observation.state.subState}`,
    `运行用户  UID ${observation.uid}`, `未登录运行条件  ${observation.scope === "system" ? "系统级服务无需 linger" : observation.linger === null ? "未知" : observation.linger ? "已满足" : "未满足"}`,
    ...(observation.source ? [`注册来源  ${observation.source.executable}`] : []),
    ...(observation.state.result && observation.state.result !== "success" ? [`服务结果  ${observation.state.result}`] : [])];
}

function query(program: string, args: string[]): Promise<{ code: number | null; output: string }> {
  return new Promise((accept, reject) => {
    const child = spawn(program, args, { stdio: ["ignore", "pipe", "ignore"], env: { ...process.env, LC_ALL: "C", SYSTEMD_PAGER: "", SYSTEMD_COLORS: "0" } });
    const chunks: Buffer[] = []; let size = 0, settled = false;
    const fail = () => {
      if (settled) return;
      settled = true; clearTimeout(timer); child.kill("SIGTERM"); child.stdout.destroy(); child.unref();
      reject(new ClientError("service_unavailable", `${program} 查询不可用或超时；系统服务状态未知。`));
    };
    const timer = setTimeout(fail, 5000);
    child.on("error", fail);
    child.stdout.on("data", (chunk: Buffer) => { size += chunk.length; if (size > 65536) fail(); else chunks.push(chunk); });
    child.on("close", (code) => {
      if (settled) return;
      settled = true; clearTimeout(timer); accept({ code, output: Buffer.concat(chunks).toString("utf8") });
    });
  });
}
