import test from "node:test";
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { mkdtempSync, existsSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { validServerName } from "../dist/config/index.js";

test("访问配置接受 IPv4、IPv6 和域名，不按地址类别拒绝", () => {
  for (const name of ["127.0.0.1", "172.16.20.4", "0.0.0.0", "::", "::1", "[::1]", "2001:db8::1", "localhost", "RUNTIME.EXAMPLE", "例子.测试"])
    assert.equal(validServerName(name), true, name);
  for (const name of ["", "http://runtime.example", "runtime.example:7240", "127.0.0.1:7240", "[::1]:7240", "[127.0.0.1]", "runtime.example/path", "runtime.example?x=1", "user@runtime.example", "999.999.999.999", "runtime example"])
    assert.equal(validServerName(name), false, name);
});
const entry = new URL("../dist/cli.js", import.meta.url);
function run(args, env = {}) {
  const root = mkdtempSync(join(tmpdir(), "ez-client-command-"));
  const home = join(root, "home");
  try {
    const result = spawnSync(process.execPath, [entry.pathname, ...args], { encoding: "utf8", timeout: 5000,
      env: { PATH: process.env.PATH, HOME: root, TERM: "dumb", NO_COLOR: "1", EZ_ASSISTANT_RUNTIME_HOME: home, ...env } });
    assert.ifError(result.error); assert.equal(existsSync(home), false);
    assert.doesNotMatch(result.stdout + result.stderr, /\u001b\[/);
    return { code: result.status, output: result.stdout + result.stderr };
  } finally { rmSync(root, { recursive: true, force: true }); }
}
test("帮助、版本及参数错误不读取或创建 Home", () => {
  for (const args of [[], ["--help"], ["start", "--help"], ["config", "--help"], ["--version"]]) assert.equal(run(args).code, 0);
  assert.match(run(["--version"]).output, /0.25.2/);
  for (const args of [["unknown"], ["status", "--unknown"], ["--runtime-home", "relative", "start"], ["start", "--timeout", "59"], ["restart", "--timeout", "NaN"], ["stop", "--timeout", "29"]]) assert.equal(run(args).code, 2);
});
test("无 Host 的静态查询和 stop 成功，restart/web 失败且不隐式启动", () => {
  assert.equal(run(["status"]).code, 0); assert.match(run(["status"]).output, /未启动/);
  assert.equal(run(["stop"]).code, 0);
  assert.equal(run(["restart"]).code, 1); assert.equal(run(["web"]).code, 1);
  assert.equal(run(["config"]).code, 2);
});
