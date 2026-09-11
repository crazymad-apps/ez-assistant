import http from "node:http";
import https from "node:https";
import * as tls from "node:tls";
import { randomUUID } from "node:crypto";
import { compatibilityHeaders, type HostAccessCommand, type HostAccessStatus, type RuntimeHostHealth, type RuntimeHostCapabilities } from "@ez-assistant/protocol/node";
import { Cancelled, ClientError, object, errorFromHost } from "../errors.js";
import type { Discovery } from "./discovery.js";

/** 使用原生 Agent，不读取代理设置，不跟随重定向；凭据只发送至固定 loopback origin。 */
export function request(target: Discovery, path: string, body?: unknown, signal?: AbortSignal): Promise<unknown> {
  return new Promise((accept, reject) => {
    const url = new URL(path, target.address);
    const secure = url.protocol === "https:";
    // Node 22.15+ 合并系统 CA；22.12–22.14 使用 Node 默认信任及 NODE_EXTRA_CA_CERTS，始终校验证书。
    const agent = secure ? new https.Agent({ keepAlive: false, rejectUnauthorized: true,
      ...(typeof tls.getCACertificates === "function" ? {
        ca: [...new Set([...tls.getCACertificates("default"), ...tls.getCACertificates("system")])],
      } : {}),
    }) : new http.Agent({ keepAlive: false });
    const req = (secure ? https : http).request(url, { agent, method: body === undefined ? "GET" : "POST", headers: {
      authorization: `Bearer ${target.access_token}`, ...compatibilityHeaders(), ...(body === undefined ? {} : { "content-type": "application/json" }),
    } });
    const timer = setTimeout(() => req.destroy(new Error("timeout")), 3000);
    const cancel = () => req.destroy(new Cancelled());
    signal?.addEventListener("abort", cancel, { once: true });
    req.on("close", () => { clearTimeout(timer); signal?.removeEventListener("abort", cancel); agent.destroy(); });
    req.on("error", (error) => reject(error instanceof Cancelled ? error : new ClientError("connection_unknown", secure ? "HTTPS 连接失败，请核对证书信任、地址与 Host 状态；结果待查询。" : "Host 连接失败或请求超时，结果待查询。")));
    req.on("response", (response) => {
      const chunks: Buffer[] = []; let size = 0;
      response.on("data", (chunk: Buffer) => { size += chunk.length; if (size > 1024 * 1024) req.destroy(new Error("oversized")); else chunks.push(chunk); });
      response.on("end", () => {
        try {
          if ((response.statusCode ?? 500) >= 300 && (response.statusCode ?? 500) < 400) throw new ClientError("redirect", "Host 返回重定向，已拒绝转发本机凭据。");
          const value: unknown = JSON.parse(Buffer.concat(chunks).toString("utf8"));
          if (response.statusCode !== 200) throw errorFromHost(value);
          accept(value);
        } catch (error) { reject(error instanceof ClientError ? error : new ClientError("invalid_response", "Host 响应无效，状态无法核实。")); }
      });
    });
    if (signal?.aborted) cancel();
    req.end(body === undefined ? undefined : JSON.stringify(body));
  });
}
export async function health(target: Discovery, signal?: AbortSignal): Promise<RuntimeHostHealth> {
  const row = object(await request(target, "/health", undefined, signal));
  if (!["starting", "ready", "unavailable"].includes(String(row.status))) throw new ClientError("invalid_response", "Host 就绪状态无效。");
  return row as unknown as RuntimeHostHealth;
}
export async function capabilities(target: Discovery, signal?: AbortSignal): Promise<RuntimeHostCapabilities> {
  const row = object(await request(target, "/capabilities", undefined, signal));
  return row as unknown as RuntimeHostCapabilities;
}
export function accessStatus(value: unknown): HostAccessStatus {
  const row = object(value), config = object(row.configuration);
  if (!(row.revision === null || typeof row.revision === "string") || typeof row.password_configured !== "boolean"
    || !["http", "https"].includes(String(config.scheme)) || !Number.isInteger(config.port) || Number(config.port) < 1 || Number(config.port) > 65535
    || typeof config.remote_enabled !== "boolean" || !Array.isArray(config.server_names) || config.server_names.some((n: unknown) => typeof n !== "string")
    || ![config.tls_certificate, config.tls_private_key].every((p) => p === null || typeof p === "string")
    || typeof row.restart_required !== "boolean" || !["closed", "listening", "failed"].includes(String(row.listener_state))
    || !(row.error === null || typeof row.error === "string")) throw new ClientError("invalid_response", "Host 访问配置响应无效。");
  return row as unknown as HostAccessStatus;
}
export async function accessCommand(target: Discovery, command: HostAccessCommand): Promise<HostAccessStatus> {
  const id = randomUUID();
  const response = object(await request(target, "/commands", { request_id: id, command: { scope: "host_access", payload: command } }));
  const result = object(response.result);
  if (response.request_id !== id || result.scope !== "host_access") throw new ClientError("invalid_response", "Host 配置响应与请求不匹配，结果待查询。");
  return accessStatus(result.payload);
}
export async function shutdown(target: Discovery, signal: AbortSignal): Promise<void> {
  const id = randomUUID();
  const response = object(await request(target, "/commands", { request_id: id, command: { scope: "runtime", payload: { type: "shutdown_runtime", payload: {} } } }, signal));
  if (response.request_id !== id || object(object(response.result).payload).type !== "shutdown_runtime") throw new ClientError("invalid_response", "停止回执不匹配，结果待查询。");
}
