import { SOFTWARE_VERSION, MIN_COMPATIBLE_VERSION } from "./generated/assistant-protocol.ts";
import type { ClientCompatibility, RuntimeCompatibilityError, RuntimeCompatibilityErrorCode } from "./generated/assistant-protocol.ts";

/** 不读取 Host 响应来构造页面版本；每个产品消费自己的构建常量。 */
export function currentCompatibility(): ClientCompatibility {
  return { version: SOFTWARE_VERSION, min_compatible_version: MIN_COMPATIBLE_VERSION };
}

export function compatibilityHeaders(): Record<string, string> {
  return { "x-ez-client-version": SOFTWARE_VERSION, "x-ez-min-compatible-version": MIN_COMPATIBLE_VERSION };
}

function parse(value: unknown): number[] | null {
  if (typeof value !== "string" || value.trim() !== value || !/^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$/.test(value)) return null;
  const parts = value.split(".").map(Number);
  return parts.some((part) => part > 0xffffffff) ? null : parts;
}

function compare(left: number[], right: number[]): number {
  return left.map((value, index) => value - right[index]).find((value) => value !== 0) ?? 0;
}

function declaration(value: unknown): ClientCompatibility | null {
  if (!value || typeof value !== "object") return null;
  const candidate = value as Record<string, unknown>;
  const version = parse(candidate.version), minimum = parse(candidate.min_compatible_version);
  if (!version || !minimum || compare(minimum, version) > 0) return null;
  return { version: candidate.version as string, min_compatible_version: candidate.min_compatible_version as string };
}

/** 与 Rust 同源向量测试；未知数据必须先校验，错误中只保留有效版本对。 */
export function checkCompatibility(client: unknown, host: unknown): RuntimeCompatibilityError | null {
  const c = declaration(client), h = declaration(host);
  let code: RuntimeCompatibilityErrorCode;
  if (!h) code = "invalid_declaration";
  else if (client == null) code = "missing_declaration";
  else if (!c) code = "invalid_declaration";
  else if (compare(parse(c.version)!, parse(h.min_compatible_version)!) < 0) code = "client_too_old";
  else if (compare(parse(h.version)!, parse(c.min_compatible_version)!) < 0) code = "host_too_old";
  else return null;
  return { code, client: c, host: h };
}
