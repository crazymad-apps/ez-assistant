#!/usr/bin/env node
import { Command, CommanderError } from "commander";
import { SOFTWARE_VERSION } from "@ez-assistant/protocol/node";
import { ClientError, Cancelled } from "./errors.js";
import { canonicalHome } from "./host/discovery.js";
import { HostControl } from "./host/control.js";
import { accessCommand } from "./host/http.js";
import { configure } from "./config/index.js";
import { line, showTarget, targetHome, working } from "./terminal/output.js";
import { openBrowser } from "./platform/browser.js";
import { querySystemd, serviceLines } from "./platform/systemd/query.js";

const abort = new AbortController();
const interrupt = () => abort.abort();
process.on("SIGINT", interrupt);
process.on("SIGTERM", interrupt);
let home: string | undefined;
const program = new Command().name("ez-assistant").description("EZ Assistant · 本机 Host 管理")
  .version(SOFTWARE_VERSION, "-V, --version", "显示 Client 软件版本")
  .option("--runtime-home <路径>", "本机 Host 的绝对 Runtime Home")
  .option("--no-color", "关闭颜色")
  .helpOption("-h, --help", "显示帮助").exitOverride().showHelpAfterError()
  .addHelpText("after", "\n首次使用 Web：config 设置访问密码 → start → web\nHost 与 Desktop 共用；Client 退出不会停止 Host。");
const descriptions = { start: "启动或复用 Host，等待业务就绪", status: "查询当前 Host 与访问设置", stop: "受控停止共享 Host", restart: "按当前 Host 来源重启", config: "交互配置 Host 访问与启动设置", web: "打开当前 Host 的普通 Web 登录页" };
for (const [name, description] of Object.entries(descriptions)) {
  const command = program.command(name).description(description);
  if (["start", "stop", "restart"].includes(name)) command.option("--timeout <秒>", name === "restart" ? "分别延长停止和启动等待（至少 60 秒；默认 30／60）" : `延长等待（至少 ${name === "stop" ? 30 : 60} 秒）`);
  command.action(async (options: { timeout?: string }) => {
    if (!["darwin", "linux"].includes(process.platform)) throw new ClientError("platform", "本版 Client 仅支持 macOS 和 Linux。");
    const minimum = name === "stop" ? 30 : 60;
    const timeout = options.timeout === undefined ? undefined : Number(options.timeout);
    if (timeout !== undefined && (!/^\d+$/.test(options.timeout!) || !Number.isSafeInteger(timeout) || timeout < minimum || timeout > 86400)) throw new ClientError("usage", `等待秒数必须为 ${minimum}–86400 的整数。`, 2);
    if (name === "config" && (!process.stdin.isTTY || !process.stdout.isTTY || process.env.TERM === "dumb")) throw new ClientError("usage", "config 需要交互式终端，请在终端中直接运行 ez-assistant config。", 2);
    home = await canonicalHome(program.opts<{ runtimeHome?: string }>().runtimeHome);
    targetHome(home);
    const host = new HostControl(home, abort.signal, line);
    if (name === "config") { if (await configure(host)) process.exitCode = 1; return; }
    if (name === "start") {
      const result = await working("正在查找或启动本机 Host", () => host.start(timeout));
      showTarget(result.target); await showAccess(result.target.discovery); line(result.reused ? "已复用现有 Host" : "Host 已就绪，Client 退出后继续后台运行"); return;
    }
    if (name === "stop" || name === "restart") {
      line("注意：将停止共享 Host，所有客户端连接与运行中任务都会受影响。");
      if (name === "stop") line(await working("正在受控停止 Host", () => host.stop(timeout)) ? "Host 已停止" : "Host 已停止，无需操作");
      else { const restarted = await working("正在校验来源并重启 Host", () => host.restart(timeout)); showTarget(restarted); await showAccess(restarted.discovery); line("Host 已沿原来源重新就绪"); }
      return;
    }
    // 即使 Host 版本不兼容，独立的系统服务状态仍可只读查询。
    if (name === "status") for (const item of serviceLines(await querySystemd(home))) line(item);
    const target = await host.status(); showTarget(target);
    if (name === "status") {
      if (target?.health.status === "ready") {
        await showAccess(target.discovery);
      }
      return;
    }
    if (!target || target.health.status !== "ready") throw new ClientError("not_ready", "Host 尚未就绪，无法打开 Web；请先 start 或查询 status。");
    const access = await accessCommand(target.discovery, { type: "get_status" });
    if (!access.password_configured) throw new ClientError("password_required", "请先通过 config 设置访问密码，再打开 Web 登录页。");
    line(`Web 登录地址  ${target.discovery.address}（仅此机器本机）`);
    if (!access.configuration.remote_enabled) line("非本地访问关闭；其他设备访问需在 config 中配置。");
    line(await openBrowser(target.discovery.address) ? "已请求浏览器打开" : "未能请求浏览器打开，请手动访问上述地址。");
  });
}
program.action(() => program.outputHelp());
try { await program.parseAsync(process.argv); }
catch (error) {
  if (error instanceof CommanderError) process.exitCode = error.exitCode ? 2 : 0;
  else if (error instanceof Cancelled || abort.signal.aborted) { line("已取消等待。未提交草稿已丢弃；已提交操作可能继续，取消不代表撤销。"); process.exitCode = 130; }
  else { line(`失败：${error instanceof ClientError ? error.message : "操作未完成，请重新查询状态。"}`); process.exitCode = error instanceof ClientError ? error.exitCode : 1; }
  if (home) line(`查询命令：ez-assistant --runtime-home '${home.replaceAll("'", "'\\''")}' status`);
} finally { process.removeListener("SIGINT", interrupt); process.removeListener("SIGTERM", interrupt); }

async function showAccess(target: import("./host/discovery.js").Discovery): Promise<void> {
  const access = await accessCommand(target, { type: "get_status" });
  line(`访问范围  ${access.configuration.remote_enabled ? "允许非本地访问" : "仅本机"}`);
  if (access.restart_required) line(`待重启设置  ${access.configuration.scheme.toUpperCase()} 端口 ${access.configuration.port}`);
}
