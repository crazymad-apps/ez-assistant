import * as p from "@clack/prompts";
import { isAbsolute } from "node:path";
import { isIP } from "node:net";
import type { HostAccessConfiguration } from "@ez-assistant/protocol/node";
import { Cancelled, ClientError, cancelled } from "../errors.js";
import { HostControl } from "../host/control.js";
import { note, safe } from "../terminal/output.js";
import { configureStartup } from "./startup.js";

type Action = "password" | "remote" | "port" | "transport" | "domains" | "review" | "save" | "reload" | "back";
export function answer<T>(value: T | symbol): T { if (p.isCancel(value)) throw new Cancelled(); return value as T; }
export function endpointChanged(a: HostAccessConfiguration, b: HostAccessConfiguration): boolean {
  return a.port !== b.port || a.scheme !== b.scheme || a.tls_certificate !== b.tls_certificate || a.tls_private_key !== b.tls_private_key;
}
export function validServerName(name: string): boolean {
  if (name.startsWith("[") && name.endsWith("]")) return isIP(name.slice(1, -1)) === 6;
  if (isIP(name)) return true;
  if (!name || /[\s/\\:?#@]/u.test(name)) return false;
  try { return !!new URL(`http://${name}`).hostname; } catch { return false; }
}
function fields(config: HostAccessConfiguration): Array<[string, string]> {
  return [["非本地访问", config.remote_enabled ? "开启" : "关闭，仅本机"], ["监听端口", String(config.port)], ["访问协议", config.scheme.toUpperCase()],
    ["允许的 IP 或域名", config.server_names.join(", ") || "未配置，可使用 IP"], ["证书路径", config.tls_certificate ?? "未配置"], ["私钥路径", config.tls_private_key ?? "未配置"]];
}
function preview(current: HostAccessConfiguration, draft: HostAccessConfiguration): void {
  const before = fields(current);
  note(fields(draft).map(([label, value], i) => `${label}  ${before[i]?.[1] === value ? `${value}（不变）` : `${before[i]?.[1]} → ${value}`}`).join("\n"), "访问设置预览 · 尚未保存");
}
export async function configure(host: HostControl): Promise<boolean> {
  const signal = host.signal;
  const saved: string[] = []; let failed = false;
  const confirm = async (message: string, initialValue: boolean) => answer(await p.confirm({ message, initialValue, active: "是", inactive: "否", signal }));
  try {
    p.intro("EZ Assistant · 本机 Host 配置");
    while (true) {
      cancelled(signal);
      const root = answer(await p.select({ message: "选择配置范围", showInstructions: false, signal, options: [
        { value: "access", label: "Host 访问设置" }, { value: "startup", label: "启动设置" }, { value: "exit", label: "退出配置" },
      ] }));
      if (root === "exit") { p.outro("已退出配置"); return failed; }
      if (root === "startup") { failed = await configureStartup(host.home, signal, () => saved.push("启动设置")); continue; }
      let session = await host.configuration();
      let draft = structuredClone(session.status.configuration); let blocked = false;
      p.log.info(session.target ? "Host 业务就绪 · 在线配置" : "Host 未启动 · 离线配置，保存供下次启动使用");
      p.log.info("↑↓ 选择 · Enter 确定 · Esc / Ctrl+C 取消配置");
      note(fields(draft).map(([key, value]) => `${key}  ${value}`).join("\n"), "当前访问设置");
      access: while (true) {
        cancelled(signal);
        const dirty = JSON.stringify(draft) !== JSON.stringify(session.status.configuration);
        const action = answer(await p.select<Action>({ message: blocked ? "配置状态已变化，请重新读取" : dirty ? "访问设置 · 有未保存修改" : "选择要配置的项目", signal, showInstructions: false, options: [
          { value: "password", label: "访问密码", hint: session.status.password_configured ? "已设置 · 独立保存" : "未设置" },
          { value: "remote", label: "非本地访问", hint: draft.remote_enabled ? "开启" : "仅本机" },
          { value: "port", label: "监听端口", hint: String(draft.port) },
          { value: "transport", label: "协议与证书", hint: draft.scheme.toUpperCase() },
          { value: "domains", label: "允许的 IP 或域名" }, { value: "review", label: "查看配置预览" },
          { value: "save", label: "保存访问设置" }, { value: "reload", label: "重新读取访问设置" }, { value: "back", label: "返回" },
        ] }));
        if (blocked && !["reload", "review", "back"].includes(action)) { p.log.warn("请先重新读取，原草稿只可预览；未自动重试提交。"); continue; }
        try {
          switch (action) {
            case "back":
              if (dirty && !await confirm("放弃未保存访问设置并返回？", false)) break;
              break access;
            case "reload":
              if (!await confirm("重新读取会丢弃当前访问草稿，继续？", false)) break;
              session = await host.configuration(); draft = structuredClone(session.status.configuration); blocked = false; failed = false;
              p.log.success("已重新读取访问设置"); break;
            case "review": preview(session.status.configuration, draft); break;
            case "password": {
              const password = answer(await p.password({ message: "设置访问密码", mask: "●", signal, validate(value) {
                if (!value?.trim()) return "密码不能为空或全空白。";
                if (Buffer.byteLength(value, "utf8") > 1024) return "密码最多 1024 字节。";
              } }));
              answer(await p.password({ message: "再次输入访问密码", mask: "●", signal, validate(value) { if (value !== password) return "两次输入不一致，请重新输入。"; } }));
              if (!await confirm("保存新密码后，已有普通登录将失效。其他访问设置仍需单独保存。", true)) break;
              await session.save({ type: "set_password", payload: { expected_revision: session.status.revision, password } });
              saved.push("访问密码"); failed = false;
              p.log.success(session.target ? "密码已更新，已有普通登录已失效。" : "密码已保存，供下次启动使用。");
              p.log.info("其他访问草稿仍保留，保存前请重新预览。"); break;
            }
            case "remote":
              if (!session.status.password_configured && !draft.remote_enabled) { p.log.warn("请先设置访问密码，再开启非本地访问。"); break; }
              if (session.target && !draft.remote_enabled && (session.status.restart_required || endpointChanged(draft, session.status.configuration))) { p.log.warn("请先保存监听配置并重启 Host，再开启非本地访问。"); break; }
              draft.remote_enabled = await confirm("允许其他设备访问本机 Host？", draft.remote_enabled); break;
            case "port":
              if (session.target && session.status.configuration.remote_enabled) { p.log.warn("请先关闭非本地访问并保存，再修改监听配置。"); break; }
              draft.port = Number(answer(await p.text({ message: "监听端口", initialValue: String(draft.port), placeholder: "7240", signal, validate(value) {
                if (!value || !/^\d+$/.test(value) || Number(value) < 1 || Number(value) > 65535) return "请输入 1–65535 之间的整数端口。";
              } }))); break;
            case "transport": {
              if (session.target && session.status.configuration.remote_enabled) { p.log.warn("请先关闭非本地访问并保存，再修改监听配置。"); break; }
              const scheme = answer(await p.select<"http" | "https">({ message: "选择访问协议", initialValue: draft.scheme, signal, showInstructions: false, options: [{ value: "http", label: "HTTP" }, { value: "https", label: "HTTPS" }] }));
              if (scheme === "http") { draft = { ...draft, scheme, tls_certificate: null, tls_private_key: null }; break; }
              const validate = (value: string | undefined) => !value || !isAbsolute(value) || /[\u0000-\u001f\u007f]/.test(value) ? "请输入绝对文件路径。" : undefined;
              const certificate = answer(await p.text({ message: "HTTPS 证书路径", initialValue: draft.tls_certificate ?? "", placeholder: "/path/host.crt", validate, signal }));
              const key = answer(await p.text({ message: "HTTPS 私钥路径", initialValue: draft.tls_private_key ?? "", placeholder: "/path/host.key", validate, signal }));
              draft = { ...draft, scheme, tls_certificate: certificate, tls_private_key: key }; break;
            }
            case "domains":
              draft.server_names = answer(await p.text({ message: "允许的 IP 或域名（逗号分隔，可留空）", initialValue: draft.server_names.join(", "), placeholder: "127.0.0.1, ::1, assistant.example.com", signal, validate(value) {
                const names = value?.split(",").map((name) => name.trim()).filter(Boolean) ?? [];
                if (names.length > 16 || names.some((name) => !validServerName(name))) return "最多 16 个合法 IP 或域名，不含协议、端口或路径。";
              } })).split(",").map((name) => name.trim()).filter(Boolean); break;
            case "save":
              if (!dirty) { p.log.info("访问设置没有变化。"); break; }
              if (session.target && draft.remote_enabled && endpointChanged(draft, session.status.configuration)) { p.log.warn("请先关闭非本地访问，修改监听并重启后再开启。"); break; }
              preview(session.status.configuration, draft);
              if (!await confirm("保存这些访问设置？", true)) break;
              await session.save({ type: "configure", payload: { expected_revision: session.status.revision, configuration: draft } });
              draft = structuredClone(session.status.configuration); saved.push("访问设置"); failed = false;
              if (!session.target) p.log.success("已保存，供下次启动使用。");
              else if (session.status.restart_required) p.log.warn("已保存，需显式 restart 后生效；当前地址保持不变。");
              else p.log.success("访问策略已即时生效。");
              break;
          }
        } catch (error) {
          if (!(error instanceof ClientError)) throw error;
          failed = true; p.log.error(safe(error.message));
          blocked = error.code !== "invalid_request" && error.code !== "port_in_use";
        }
      }
    }
  } catch (error) {
    if (saved.length) p.log.info(`此前已保存：${[...new Set(saved)].join("、")}；不会因退出撤销。`);
    throw error;
  }
}
