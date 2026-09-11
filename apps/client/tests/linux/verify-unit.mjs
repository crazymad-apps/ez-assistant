// 专用 Docker 测试用户下验证实际 systemd 路径解析；执行的是一次性 argv 夹具，不是业务 Host。
import assert from "node:assert/strict";
import { mkdtemp, mkdir, writeFile, readFile, unlink } from "node:fs/promises";
import { homedir, userInfo } from "node:os";
import { join } from "node:path";
import { execFileSync } from "node:child_process";
import { setTimeout as delay } from "node:timers/promises";
import { unitName, renderUnit } from "../../dist/platform/systemd/unit.js";

assert.equal(process.platform, "linux");
assert.equal(userInfo().username, "eztest", "仅限专用容器测试用户");
const userHome = homedir();
assert.equal(userHome, "/home/eztest");
const root = await mkdtemp(join(userHome, "unit-validation-"));
const runtimeHome = join(root, '中文 %n $HOME "\\ runtime');
const executable = join(root, '中文 %n fixture');
await mkdir(runtimeHome, { mode: 0o700 });
await writeFile(executable, '#!/usr/bin/python3\nimport json,os,pathlib,sys\npathlib.Path(__file__).with_name("observed.json").write_text(json.dumps({"argv":sys.argv[1:],"cwd":os.getcwd(),"HOME":os.environ.get("HOME")}))\n', { mode: 0o700 });
const unit = unitName(runtimeHome);
const directory = join(userHome, ".config/systemd/user");
await mkdir(directory, { recursive: true, mode: 0o700 });
const path = join(directory, unit);
await writeFile(path, renderUnit({ runtimeHome, executable, userHome }), { mode: 0o600, flag: "wx" });
const systemctl = (...args) => execFileSync("systemctl", ["--user", "--no-pager", ...args], { encoding: "utf8", timeout: 10000 });
execFileSync("systemd-analyze", ["--user", "verify", path], { stdio: "pipe", timeout: 10000 });
systemctl("daemon-reload");
assert.equal(systemctl("show", unit, "--property=UnitFileState", "--value").trim(), "disabled");
systemctl("start", unit);
let observed;
for (let attempt = 0; attempt < 30; attempt++) {
  try { observed = JSON.parse(await readFile(join(root, "observed.json"), "utf8")); break; }
  catch (error) { if (error.code !== "ENOENT") throw error; await delay(100); }
}
assert.deepEqual(observed, { argv: ["serve", "--runtime-home", runtimeHome], cwd: runtimeHome, HOME: userHome });
for (let attempt = 0; attempt < 30 && systemctl("show", unit, "--property=MainPID", "--value").trim() !== "0"; attempt++) await delay(100);
assert.equal(systemctl("show", unit, "--property=MainPID", "--value").trim(), "0");
assert.equal(systemctl("show", unit, "--property=Result", "--value").trim(), "success");
await writeFile(join(root, "unit.service"), await readFile(path));
await unlink(path);
systemctl("daemon-reload");
console.log(`PASS actual systemd: Unicode, spaces, percent, dollar, quote and backslash paths; retained evidence ${root}`);
