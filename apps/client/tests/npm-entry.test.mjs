import test from "node:test";
import assert from "node:assert/strict";
import { mkdtemp, readFile, access, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { spawnSync } from "node:child_process";

test("npm 入口在加载依赖前拒绝低于 API 下限或未支持的 Node 主版本", async () => {
  const entry = new URL("../dist/distribution/npm-entry.js", import.meta.url);
  for (const version of ["20.19.0", "22.0.0", "22.11.9", "23.11.0", "25.0.0"]) {
    // 单独子进程只替换版本描述，验证早期拒绝；受支持版本通过真实 Node 工具链执行下方用例。
    const source = `Object.defineProperty(process.versions, "node", { value: ${JSON.stringify(version)} }); await import(${JSON.stringify(entry.href)});`;
    const result = spawnSync(process.execPath, ["--input-type=module", "-e", source], { encoding: "utf8" });
    assert.equal(result.status, 1, result.stderr);
    assert.match(result.stderr, /需要 Node 22\.12\+（22\.x）或 24\.x/);
    assert.ok(result.stderr.includes(version));
    assert.doesNotMatch(result.stderr, /SyntaxError|TypeError|ERR_MODULE/);
  }
});

test("npm 入口使用单参数 shebang，保留参数且不创建 Runtime Home", async () => {
  const root = await mkdtemp(join(tmpdir(), "ez-client-entry-"));
  const entry = fileURLToPath(new URL("../dist/distribution/npm-entry.js", import.meta.url));
  try {
    assert.equal((await readFile(entry, "utf8")).split("\n")[0], "#!/usr/bin/env node");
    // tsc 输出无可执行位，测试通过现有解释器执行；正式 npm pack 会赋予入口可执行位。
    for (const [args, code, expected] of [[['--version'], 0, /0\.25\.2/], [['--help'], 0, /本机 Host 管理/], [['--unknown-flag'], 2, /unknown option/]]) {
      const result = spawnSync(process.execPath, [entry, ...args], { env: { ...process.env, EZ_ASSISTANT_RUNTIME_HOME: join(root, 'missing') }, encoding: 'utf8' });
      assert.equal(result.status, code, result.stderr);
      assert.match(result.stdout + result.stderr, expected);
    }
    await assert.rejects(access(join(root, 'missing')), { code: 'ENOENT' });
  } finally { await rm(root, { recursive: true, force: true }); }
});
