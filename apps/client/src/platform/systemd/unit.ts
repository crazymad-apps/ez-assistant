import { createHash } from "node:crypto";
import { isAbsolute } from "node:path";
import { ClientError } from "../../errors.js";

/** systemd 原生文件契约，不保存 Client 偏好或依赖 Client 的执行入口。 */
export interface UnitSource { runtimeHome: string; executable: string; userHome: string; }

export function unitName(canonicalHome: string): string {
  absolute(canonicalHome);
  return `ez-assistant-${createHash("sha256").update(canonicalHome).digest("hex")}.service`;
}

export type ServiceScope = "user" | "system";
export function serviceScope(): ServiceScope { return process.getuid?.() === 0 ? "system" : "user"; }
export function managerArgs(scope: ServiceScope): string[] { return scope === "system" ? [] : ["--user"]; }

export function renderUnit(source: UnitSource, scope: ServiceScope = "user"): string {
  for (const path of Object.values(source)) absolute(path);
  // ExecStart 的首个参数不支持环境变量替换；拒绝歧义路径，不能把它当 shell 转义。
  if (/[$'"\\]/.test(source.executable)) throw conflict("Host 可执行路径不能包含 $、引号或反斜杠，请使用固定安装目录。");
  // systemd 255 执行器序列化会去掉 WorkingDirectory 的尾部空格，不能生成无法启动的服务。
  if (source.runtimeHome.endsWith(" ")) throw conflict("自启服务的 Runtime Home 不能以空格结尾。");
  // WorkingDirectory 是原始路径字段，不接受 argv 引号；末尾 /. 避免反斜杠变成续行。
  return `[Unit]
Description=ez-assistant Runtime Host

[Service]
Type=${scope === "system" ? "simple" : "exec"}${scope === "system" ? "\nUser=0" : ""}
ExecStart=${quote(source.executable)} serve --runtime-home ${quote(source.runtimeHome, true)}
WorkingDirectory=${source.runtimeHome.replaceAll("%", "%%")}/.
UMask=0077
Restart=no
KillSignal=SIGTERM
KillMode=mixed
SendSIGKILL=no
TimeoutStopSec=30s
StandardOutput=journal
StandardError=journal
Environment=${quote(`HOME=${source.userHome}`)} "PATH=/usr/local/bin:/usr/bin:/bin" "LANG=C.UTF-8"

[Install]
WantedBy=${scope === "system" ? "multi-user.target" : "default.target"}
`;
}

/** 只接受本产品的完整模板；人工扩展和未知指令交给冲突处理，不能自动覆盖。 */
export function parseUnit(text: string, runtimeHome: string, userHome: string, scope: ServiceScope = "user"): UnitSource {
  if (Buffer.byteLength(text, "utf8") > 65536) throw conflict("服务文件超过允许范围。");
  const match = /^ExecStart=("(?:[^"\\\n]|\\.)*") serve --runtime-home ("(?:[^"\\\n]|\\.)*")$/m.exec(text);
  if (!match?.[1] || !match[2]) throw conflict("服务启动命令不符合本产品约定。");
  try {
    const source = { executable: unquote(match[1]), runtimeHome: unquote(match[2], true), userHome };
    if (source.runtimeHome !== runtimeHome || renderUnit(source, scope) !== text) throw conflict("服务文件的来源、环境或停止策略已被修改。");
    return source;
  } catch (error) {
    if (error instanceof ClientError) throw error;
    throw conflict("服务文件包含无法核实的路径转义。");
  }
}

export function conflict(message: string): ClientError { return new ClientError("service_conflict", message); }
function absolute(path: string): void {
  if (!isAbsolute(path) || /[\u0000-\u001f\u007f]/.test(path)) throw conflict("服务路径必须是无控制字符的绝对路径。");
}
function quote(value: string, argument = false): string {
  return JSON.stringify(value.replaceAll("%", "%%").replaceAll("$", () => argument ? "$$" : "$"));
}
function unquote(value: string, argument = false): string {
  const decoded: unknown = JSON.parse(value);
  if (typeof decoded !== "string") throw conflict("服务路径不是文本。");
  const literal = decoded.replaceAll("%%", "%");
  return argument ? literal.replaceAll("$$", () => "$") : literal;
}
