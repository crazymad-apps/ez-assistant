export class ClientError extends Error {
  constructor(readonly code: string, message: string, readonly exitCode = 1) { super(message); }
}
export class Cancelled extends Error {}
export function cancelled(signal?: AbortSignal): void { if (signal?.aborted) throw new Cancelled(); }
export function object(value: unknown): Record<string, unknown> {
  if (!value || typeof value !== "object" || Array.isArray(value)) throw new ClientError("invalid_response", "Host 返回了无法识别的数据。");
  return value as Record<string, unknown>;
}
export function errorFromHost(value: unknown): ClientError {
  const code = String(object(object(value).error).code);
  const messages: Record<string, string> = {
    configuration_conflict: "配置已被其他入口修改，请重新读取后确认。",
    configuration_unavailable: "配置当前不可用，请核对权限或重新读取。",
    busy: "Host 或另一配置提交正在持锁，请重新读取状态。",
    invalid_request: "Host 拒绝此设置，请核对密码、端口、域名、证书及非本地访问条件。",
    port_in_use: "配置端口已被占用，请调整端口后重试。",
    unauthorized: "本机认证失败，请重新查询 Host。",
    forbidden: "当前操作未获 Host 授权。",
  };
  // InvalidRequest 的 message 是 Host AccessError 提供的脱敏字段原因，不回显未知异常正文。
  const detail = object(object(value).error).message;
  if (code === "invalid_request" && typeof detail === "string" && detail.length <= 2000) return new ClientError(code, detail);
  return new ClientError(code, messages[code] ?? "Host 拒绝了请求，请重新查询状态。");
}
