// 只由 flock 内部提交入口运行；不启动 Host，不读写业务库，不承载长期服务。
import { constants } from "node:fs";
import { lstat, mkdir, open, rename, unlink } from "node:fs/promises";
import { dirname, join, parse, resolve } from "node:path";
import { userInfo } from "node:os";
import { randomUUID } from "node:crypto";
import { ClientError, object } from "../../errors.js";
import { canonicalHome, missing, privateDirectory } from "../../host/discovery.js";
import { verifySource } from "../../host/process.js";
import { querySystemd, unitDirectory } from "./query.js";
import { readUnit } from "./status.js";
import { renderUnit, conflict, managerArgs, type UnitSource } from "./unit.js";
import { serviceCommand } from "./command.js";

const completed: string[] = [];
try {
  if (process.platform !== "linux") throw conflict("自启提交只支持 Linux。");
  // 内部 stdin 有界且必须及时结束，避免异常调用无限持有提交锁。
  const chunks: Buffer[] = []; let size = 0;
  const inputTimer = setTimeout(() => process.stdin.destroy(new Error("input timeout")), 5000);
  try {
    for await (const chunk of process.stdin) {
      const bytes = Buffer.from(chunk); size += bytes.length;
      if (size > 65536) throw conflict("启动设置输入超过允许范围。");
      chunks.push(bytes);
    }
  } finally { clearTimeout(inputTimer); }
  const input = object(JSON.parse(Buffer.concat(chunks).toString("utf8")) as unknown);
  if (typeof input.home !== "string" || typeof input.enabled !== "boolean" || await canonicalHome(input.home) !== input.home) throw conflict("启动设置输入无效。");
  const home = input.home, user = userInfo();
  const current = await querySystemd(home);
  if (current.kind !== "known" || JSON.stringify(current) !== JSON.stringify(input.expected)) throw conflict("预览后的服务状态已变化，请重新读取。");
  let source: UnitSource | null = null;
  if (input.source !== null) {
    const row = object(input.source);
    if (typeof row.executable !== "string" || row.runtimeHome !== home || row.userHome !== user.homedir) throw conflict("注册来源无效。");
    source = { executable: row.executable, runtimeHome: home, userHome: user.homedir };
    renderUnit(source, current.scope);
  }
  if ((input.enabled && !source) || (current.source && !source)) throw conflict("缺少注册来源。");
  const replacing = JSON.stringify(source) !== JSON.stringify(current.source);
  if (replacing && (current.state.mainPid !== 0 || !["inactive", "failed"].includes(current.state.activeState))) throw conflict("服务仍活动，不能切换来源。");
  if (source && (input.enabled || replacing)) {
    const version = object(input.version);
    if (typeof version.version !== "string" || typeof version.min_compatible_version !== "string" || typeof input.sha256 !== "string") throw conflict("缺少来源版本和摘要。");
    await verifySource(source.executable, input.sha256, { version: version.version, min_compatible_version: version.min_compatible_version });
  }
  if (current.scope === "user" && input.enabled && current.linger !== true) {
    await serviceCommand("loginctl", ["--no-ask-password", "enable-linger", String(user.uid)]);
    completed.push("已请求开启 linger");
    const actual = await querySystemd(home);
    if (actual.kind !== "known" || actual.linger !== true) throw conflict(`linger 未确认开启；请管理员执行 loginctl enable-linger ${user.uid}。`);
  }
  if (replacing && source) {
    await ensureDirectory(home, user.uid);
    if (!await privateDirectory(home)) throw conflict("Runtime Home 必须是当前用户的私有目录。");
    const directory = await unitDirectory();
    await ensureDirectory(directory, user.uid);
    const path = join(directory, current.unit), temporary = join(directory, `.${current.unit}.${randomUUID()}`);
    const text = renderUnit(source, current.scope);
    const file = await open(temporary, constants.O_WRONLY | constants.O_CREAT | constants.O_EXCL | constants.O_NOFOLLOW, 0o600);
    let published = false;
    try {
      await file.writeFile(text); await file.sync();
      const before = await readUnit(directory, home, user.homedir, user.uid, current.scope);
      const state = await querySystemd(home);
      if ((before?.text ?? null) !== (current.source ? renderUnit(current.source, current.scope) : null)
        || state.kind !== "known" || JSON.stringify(state.state) !== JSON.stringify(current.state)) throw conflict("提交期间注册文件或服务状态已变化。");
      await rename(temporary, path); published = true; completed.push("unit 已写入");
      const folder = await open(directory, constants.O_RDONLY | constants.O_DIRECTORY | constants.O_NOFOLLOW);
      try { await folder.sync(); } finally { await folder.close(); }
    } finally {
      await file.close();
      if (!published) await unlink(temporary);
    }
    await serviceCommand("systemctl", [...managerArgs(current.scope), "--no-ask-password", "daemon-reload"]);
    completed.push("systemd 已重新加载");
  }
  // 相同开关不重复提交；来源更新仍需重新核对 enable 链接。
  const desired = input.enabled ? "enabled" : "disabled";
  if (source && (replacing || current.state.fileState !== desired)) {
    await serviceCommand("systemctl", [...managerArgs(current.scope), "--no-ask-password", input.enabled ? "enable" : "disable", current.unit]);
    completed.push(input.enabled ? "已请求 enable（未启动）" : "已请求 disable（未停止）");
  }
  const actual = await querySystemd(home);
  if (actual.kind !== "known" || JSON.stringify(actual.source) !== JSON.stringify(source)
    || actual.autostart !== (input.enabled ? "enabled" : source ? "disabled" : "unconfigured")) throw conflict("提交后状态不一致，请重新查询。");
  process.stdout.write("启动设置已保存并回读。\n");
} catch (error) {
  process.stdout.write(`${error instanceof ClientError ? error.message : "自启提交失败，后续系统操作已停止。"}\n${completed.length ? completed.join("；") : "未完成系统设置操作"}\n`);
  process.exitCode = 1;
}

/** 只在明确保存时逐层创建缺失目录；不跟随链接、不修复既有目录权限。 */
async function ensureDirectory(path: string, uid: number): Promise<void> {
  let current = parse(path).root;
  for (const part of path.slice(current.length).split("/")) {
    if (!part) continue;
    current = join(current, part);
    try { await lstat(current); } catch (error) { if (!missing(error)) throw error; await mkdir(current, { mode: 0o700 }); }
    const metadata = await lstat(current);
    const stickyRoot = metadata.uid === 0 && (metadata.mode & 0o1000) !== 0;
    if (!metadata.isDirectory() || ![uid, 0].includes(metadata.uid) || ((metadata.mode & 0o022) && !stickyRoot)) throw conflict("服务目录类型、归属或写权限不安全。");
  }
  if (resolve(path) !== path || dirname(path) === path || (await lstat(path)).uid !== uid) throw conflict("服务目标目录必须属于当前用户。");
}
