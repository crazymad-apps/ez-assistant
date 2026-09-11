import { checkCompatibility, currentCompatibility } from "@ez-assistant/protocol";
import type { RuntimeCompatibilityError, RuntimeCompatibilityErrorCode, RuntimeHostCapabilities } from "@ez-assistant/protocol";

export function hostCompatibilityError(host: RuntimeHostCapabilities): RuntimeCompatibilityError | null {
  return checkCompatibility(currentCompatibility(), { version: host?.runtime_version, min_compatible_version: host?.min_compatible_version });
}

export function compatibilityMessage(code: RuntimeCompatibilityErrorCode): string {
  const messages: Record<RuntimeCompatibilityErrorCode, string> = {
    missing_declaration: "客户端缺少软件版本声明，请更新应用或刷新页面。",
    invalid_declaration: "客户端或 Host 的软件版本声明无效，请更新对应应用或刷新页面。",
    client_too_old: "客户端低于 Host 的最低兼容版本，请更新应用或刷新页面。",
    host_too_old: "Host 低于客户端的最低兼容版本，请更新 Host。",
  };
  return messages[code];
}

export function isCompatibilityCode(value: unknown): value is RuntimeCompatibilityErrorCode {
  return typeof value === "string" && ["missing_declaration", "invalid_declaration", "client_too_old", "host_too_old"].includes(value);
}
