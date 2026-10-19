import type { HostLoginResult, HostRegistrationCommand, HostRegistrationStatus } from "@ez-assistant/protocol/node";
import { ClientError, object } from "../errors.js";
import type { Discovery } from "./discovery.js";
import { request } from "./http.js";

export async function registration(
  target: Discovery,
  command: HostRegistrationCommand,
  signal?: AbortSignal,
  token?: string,
): Promise<HostRegistrationStatus> {
  const value = object(await request(target, "/host-network/registration", command, signal, token));
  if (!("device" in value) || !("error" in value) || !("intent" in value))
    throw new ClientError("invalid_response", "设备登记状态无法核实。");
  return value as unknown as HostRegistrationStatus;
}

export async function enterpriseLogin(
  target: Discovery,
  username: string,
  password: string,
  signal: AbortSignal,
): Promise<HostLoginResult> {
  const result = object(
    await request(target, "/auth/login", { method: "enterprise", username, password, native: true }, signal),
  );
  if (result.instance_id !== target.instance_id || result.mode !== "enterprise" || typeof result.token !== "string")
    throw new ClientError("invalid_response", "企业登录响应无法核实，请查询 Host。");
  return result as unknown as HostLoginResult;
}

export function registrationLines(status: HostRegistrationStatus): string[] {
  const state = status.device?.status;
  const labels = {
    pending: "等待管理员确认",
    registered: "已登记",
    rejected: "申请已拒绝",
    withdrawn: "申请已撤回",
    revoked: "已注销",
  };
  const connection = {
    disconnected: "未连接",
    connecting: "连接中",
    online: "已连接企业中心",
    retrying: "正在重连",
    conflict: "设备身份冲突",
    incompatible: "协议不兼容",
    credential_invalid: "设备身份失效",
    failed: "连接失败",
  };
  return [
    `设备名称  ${status.device?.name ?? "尚未登记"}`,
    `登记状态  ${status.error ? "状态尚未核实" : state ? labels[state] : status.registration_id ? "申请结果待核实" : "未登记"}`,
    `连接状态  ${connection[status.connection] ?? "未知"}`,
    ...(status.intent === "exit" || status.intent === "withdraw" ? ["本机已停止目标接入，中心撤销待确认"] : []),
    ...(status.error ? [`查询反馈  ${status.error.message}`] : []),
  ];
}
