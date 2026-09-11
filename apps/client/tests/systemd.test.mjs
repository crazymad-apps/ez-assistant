import test from "node:test";
import assert from "node:assert/strict";
import { mkdtemp, realpath, writeFile, chmod, symlink, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { unitName, renderUnit, parseUnit } from "../dist/platform/systemd/unit.js";
import { parseProperties, autostartState, readUnit } from "../dist/platform/systemd/status.js";
import { querySystemd, serviceLines } from "../dist/platform/systemd/query.js";

const source = { executable: '/opt/ez/0.25.2/中文 空格%/ez-assistant-runtime', runtimeHome: '/home/test/中文 %n $HOME "\\/runtime', userHome: '/home/test' };
const state = (overrides = {}) => ({ LoadState: "loaded", UnitFileState: "enabled", ActiveState: "active", SubState: "running", MainPID: "123", Result: "success", ExecStart: "{ path=/opt/host ; argv[]=/opt/host serve ; }", FragmentPath: "/home/test/.config/systemd/user/example.service", DropInPaths: "", NeedDaemonReload: "no", ControlGroup: "", ...overrides });
const output = (row) => Object.entries(row).map(([key, value]) => `${key}=${value}`).join("\n")+"\n";

test("非 Linux 状态不访问管理器，部分失败不显示默认关闭", async () => {
  if (process.platform !== "linux") assert.equal((await querySystemd("/unused/no-host-access")).kind, "unsupported");
  assert.deepEqual(serviceLines({ kind: "unavailable", reason: "用户管理器不可用" }), ["开机自启  用户管理器不可用"]);
});

test("unit 固定直接 Host 来源、停止策略和独立环境，特殊路径可逆", () => {
  const unit = renderUnit(source);
  assert.deepEqual(parseUnit(unit, source.runtimeHome, source.userHome), source);
  assert.match(unit, /Type=exec\n/); assert.match(unit, /Restart=no\n/); assert.match(unit, /SendSIGKILL=no\n/);
  assert.match(unit, /KillMode=mixed\n/); assert.match(unit, /TimeoutStopSec=30s\n/);
  assert.ok(!unit.includes("node ")); assert.ok(!unit.includes("ExecStop=")); assert.ok(!unit.includes("EnvironmentFile="));
  assert.ok(unit.includes("%%n $$HOME"));
  assert.match(unitName(source.runtimeHome), /^ez-assistant-[0-9a-f]{64}\.service$/);
  assert.notEqual(unitName(source.runtimeHome), unitName(source.runtimeHome+"2"));
  assert.throws(() => renderUnit({ ...source, executable: "/opt/$HOST" }), { code: "service_conflict" });
  assert.throws(() => renderUnit({ ...source, runtimeHome: "/home/test/runtime " }), { code: "service_conflict" });
  for (const character of ["'", '"', "\\"]) assert.throws(() => renderUnit({ ...source, executable: `/opt/${character}host` }), { code: "service_conflict" });
  assert.throws(() => renderUnit({ ...source, runtimeHome: "/home/test\nExecStart=/bin/other" }), { code: "service_conflict" });
});

test("人工修改、错误 Home、额外命令和自动强杀配置均按冲突处理", () => {
  const unit = renderUnit(source);
  for (const changed of [unit.replace("Restart=no", "Restart=always"), unit.replace("SendSIGKILL=no", "SendSIGKILL=yes"), unit+"ExecStop=/bin/kill\n", unit.replace("Type=exec", "Type=simple")]) {
    assert.throws(() => parseUnit(changed, source.runtimeHome, source.userHome), { code: "service_conflict" });
  }
  assert.throws(() => parseUnit(unit, "/another/home", source.userHome), { code: "service_conflict" });
});

test("开机自启与当前运行独立，linger 未核实不能声明开启", () => {
  assert.equal(autostartState(parseProperties(output(state())), true, source), "enabled");
  for (const linger of [false, null]) assert.equal(autostartState(parseProperties(output(state())), linger, source), "incomplete");
  assert.equal(autostartState(parseProperties(output(state({ UnitFileState: "disabled" }))), true, source), "disabled");
  assert.equal(autostartState(parseProperties(output(state({ ActiveState: "inactive", MainPID: "0", SubState: "dead" }))), true, source), "enabled");
  const absent = parseProperties(output(state({ LoadState: "not-found", UnitFileState: "", ActiveState: "inactive", MainPID: "0", SubState: "dead", ExecStart: "", FragmentPath: "" })));
  assert.equal(autostartState(absent, null, null), "unconfigured");
  const absentWithoutCommand = output(state({ LoadState: "not-found", UnitFileState: "", ActiveState: "inactive", MainPID: "0", SubState: "dead", ExecStart: "", FragmentPath: "" })).replace("ExecStart=\n", "");
  assert.equal(autostartState(parseProperties(absentWithoutCommand), null, null), "unconfigured");
  assert.throws(() => autostartState(parseProperties(output(state())), true, null), { code: "service_conflict" });
});

test("状态缺字段、重复字段、覆盖配置和未加载改动不能退化为关闭", () => {
  for (const changed of [{ DropInPaths: "/etc/systemd/user/overrides.conf" }, { NeedDaemonReload: "yes" }, { UnitFileState: "masked" }, { MainPID: "-1" }, { FragmentPath: "" }, { LoadState: "error" }]) {
    assert.throws(() => parseProperties(output(state(changed))), { code: "service_conflict" });
  }
  assert.throws(() => parseProperties(output(state())+"MainPID=999\n"), { code: "service_conflict" });
  assert.throws(() => parseProperties(output(state()).replace("Result=success\n", "")), { code: "service_conflict" });
});

test("读取 unit 不创建文件或修复权限，拒绝替换链接和外部可写文件", async () => {
  const directory = await realpath(await mkdtemp(join(tmpdir(), "ez-client-unit-")));
  const path = join(directory, unitName(source.runtimeHome));
  try {
    assert.equal(await readUnit(directory, source.runtimeHome, source.userHome, process.getuid()), null);
    await writeFile(path, renderUnit(source), { mode: 0o600 });
    assert.deepEqual((await readUnit(directory, source.runtimeHome, source.userHome, process.getuid())).source, source);
    await chmod(path, 0o666); await assert.rejects(readUnit(directory, source.runtimeHome, source.userHome, process.getuid()), { code: "service_conflict" });
    await rm(path); await writeFile(join(directory, "other"), renderUnit(source), { mode: 0o600 }); await symlink(join(directory, "other"), path);
    await assert.rejects(readUnit(directory, source.runtimeHome, source.userHome, process.getuid()), { code: "service_conflict" });
  } finally { await rm(directory, { recursive: true, force: true }); }
});

test("系统级模板使用系统启动目标，不依赖 linger，不能按用户级模板解析", () => {
  const unit = renderUnit(source, "system");
  assert.match(unit, /Type=simple\nUser=0\n/);
  assert.match(unit, /WantedBy=multi-user.target\n/);
  assert.deepEqual(parseUnit(unit, source.runtimeHome, source.userHome, "system"), source);
  assert.throws(() => parseUnit(unit, source.runtimeHome, source.userHome), { code: "service_conflict" });
  assert.equal(autostartState(parseProperties(output(state())), null, source, "system"), "enabled");
  assert.equal(autostartState(parseProperties(output(state({ UnitFileState: "disabled" }))), null, source, "system"), "disabled");
});
