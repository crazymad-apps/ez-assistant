// 通过 stdin 驱动成品包；仅验证本轮新增服务提交／生命周期，不重复 Web 或版本回归。
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { execFileSync, spawn } from "node:child_process";
import { once } from "node:events";
const [pkg, home] = process.argv.slice(2);
assert.equal(process.platform, "linux");
assert(pkg.startsWith("/work/ez-assistant-client-") && home.startsWith("/home/eztest/ip-fix-"));
const { prepareService, saveService } = await import(`${pkg}/app/platform/systemd/commit.js`);
const { querySystemd } = await import(`${pkg}/app/platform/systemd/query.js`);
const signal = new AbortController().signal;
const save = async (enabled) => saveService(await prepareService(home, enabled), signal);
const cli = (...args) => execFileSync(`${pkg}/bin/ez-assistant`, ["--runtime-home", home, ...args], { encoding: "utf8" });
const discovery = async () => JSON.parse(await readFile(`${home}/run/runtime.json`, "utf8"));
const before = await querySystemd(home);
assert.equal(before.kind, "known"); assert.equal(before.autostart, "unconfigured");
const enabled = await save(true);
assert.equal(enabled.autostart, "enabled"); assert.equal(enabled.state.mainPid, 0);
console.log("PASS enable registers without starting Host");

const held = spawn("flock", ["--exclusive", "--no-fork", `/run/user/2000/${enabled.unit}.lock`, `${pkg}/runtime/node`, "-e", "process.stdout.write('locked');setTimeout(()=>{},10000)"], { stdio: ["ignore", "pipe", "inherit"] });
const closed = once(held, "close");
await once(held.stdout, "data");
try { await assert.rejects(save(false), { code: "service_conflict" }); }
finally { held.kill("SIGTERM"); await closed; }
console.log("PASS concurrent submission rejected by real flock");

cli("start"); const first = await discovery();
assert.equal((await querySystemd(home)).state.mainPid, first.pid);
assert.equal(first.executable_path, `${pkg}/host/ez-assistant-runtime`);
await assert.rejects(prepareService(home, true, true), { code: "service_conflict" });
const stale = await prepareService(home, true);
await save(false);
assert.equal((await discovery()).instance_id, first.instance_id);
await assert.rejects(saveService(stale, signal), { code: "service_partial" });
assert.equal((await querySystemd(home)).autostart, "disabled");
console.log("PASS disable preserves active Host; active source replacement and stale preview rejected");

cli("restart"); const restarted = await discovery();
assert.notEqual(restarted.instance_id, first.instance_id);
const running = await querySystemd(home);
assert.equal(running.state.mainPid, restarted.pid); assert.equal(running.autostart, "disabled");
assert.equal(restarted.executable_path, first.executable_path);
cli("stop");
const stopped = await querySystemd(home);
assert.equal(stopped.state.mainPid, 0); assert.equal(stopped.state.activeState, "inactive");
assert.equal(stopped.autostart, "disabled");
console.log(JSON.stringify({ result: "PASS disabled service restart keeps original unit; controlled stop settles service", home, unit: stopped.unit, active: stopped.state.activeState, autostart: stopped.autostart }));
