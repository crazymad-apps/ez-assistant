import type { HostLoginRequest, HostLoginResult, RuntimeHostCapabilities } from "../generated/assistant-protocol";
import type { RuntimeBootstrap } from "../native-bridge/runtimeBootstrap";

export class WebLoginError extends Error {
  readonly code = "authentication_required";
}

export async function loginWeb(request: HostLoginRequest): Promise<void> {
  await readResponse(await fetch("/auth/login", {
    method: "POST", credentials: "same-origin", redirect: "error",
    headers: { "Content-Type": "application/json" }, body: JSON.stringify(request),
  }));
}

export async function bootstrapWebRuntime(): Promise<RuntimeBootstrap> {
  const [session, capabilities] = await Promise.all([
    fetch("/auth/session", { credentials: "same-origin", redirect: "error", cache: "no-store" }).then(readResponse) as Promise<HostLoginResult>,
    fetch("/capabilities", { credentials: "same-origin", redirect: "error", cache: "no-store" }).then(readResponse) as Promise<RuntimeHostCapabilities>,
  ]);
  if (capabilities.protocol_version !== 2 || !capabilities.features?.includes("web_login")) {
    throw new Error("页面与 Host 版本不一致，请使用同一版本的应用。");
  }
  return { base_url: window.location.origin, instance_id: session.instance_id, access_token: "", capabilities, started_runtime: false, authentication: "web" };
}

export async function logoutWeb(): Promise<void> {
  const response = await fetch("/auth/logout", { method: "POST", credentials: "same-origin", redirect: "error" });
  if (!response.ok) throw new Error("Host 退出请求未完成。");
}

/** 在任何业务请求之前移除 fragment，token 只短暂存在于本次登录调用。 */
export function takeWebLoginToken(): string | null {
  const fragment = new URLSearchParams(window.location.hash.slice(1));
  const token = fragment.get("token");
  if (fragment.has("token")) {
    fragment.delete("token");
    const rest = fragment.toString();
    window.history.replaceState(null, "", `${window.location.pathname}${window.location.search}${rest ? `#${rest}` : ""}`);
  }
  return token;
}

async function readResponse(response: Response): Promise<unknown> {
  if (response.ok) return response.json();
  let message = response.status === 401 ? "请输入密码登录。" : "无法连接 Host，请稍后重试。";
  try {
    const body = await response.json() as { error?: { message?: unknown } };
    if (typeof body.error?.message === "string") message = body.error.message;
  } catch { /* 非 JSON 网络错误使用固定文案。 */ }
  if (response.status === 401) throw new WebLoginError(message);
  throw new Error(message);
}
