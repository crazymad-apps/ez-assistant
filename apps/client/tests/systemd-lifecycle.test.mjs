import test from "node:test";
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";

test("用户管理器不可用时，只有未注册的 Host 可手动管理；已有或不安全注册仍拒绝", () => {
  // 隔离模块替换与临时文件，不访问真实用户服务或执行 Host。
  const script = `
    import assert from "node:assert/strict";
    import { mock } from "node:test";
    import * as fs from "node:fs/promises";
    import { userInfo, tmpdir } from "node:os";
    import { join } from "node:path";
    const root = await fs.realpath(await fs.mkdtemp(join(tmpdir(), "ez-client-service-")));
    const home = join(root, "runtime"), user = userInfo();
    let observation = { kind: "unavailable", reason: "用户管理器不可用" };
    mock.module("node:fs/promises", { namedExports: { ...fs,
      lstat: async (path, ...args) => path === "/run/systemd/system" ? { isDirectory: () => true } : fs.lstat(path, ...args)
    } });
    mock.module(${JSON.stringify(new URL("../dist/platform/systemd/query.js", import.meta.url).href)}, {
      namedExports: { querySystemd: async () => observation, unitDirectory: async () => root }
    });
    const { serviceForHome } = await import(${JSON.stringify(new URL("../dist/platform/systemd/lifecycle.js", import.meta.url).href)});
    const { renderUnit, unitName } = await import(${JSON.stringify(new URL("../dist/platform/systemd/unit.js", import.meta.url).href)});
    const path = join(root, unitName(home));
    try {
      assert.equal(await serviceForHome(home), null);
      await fs.writeFile(path, renderUnit({ executable: "/opt/ez/host", runtimeHome: home, userHome: user.homedir }), { mode: 0o600 });
      await assert.rejects(serviceForHome(home), { code: "service_conflict" });
      await fs.chmod(path, 0o666);
      await assert.rejects(serviceForHome(home), { code: "service_conflict" });
      await fs.rm(path);
      await fs.symlink(join(root, "missing"), path);
      await assert.rejects(serviceForHome(home), { code: "service_conflict" });
      await fs.rm(path);
      observation = { kind: "conflict", reason: "注册冲突" };
      await assert.rejects(serviceForHome(home), { code: "service_conflict" });
    } finally { await fs.rm(root, { recursive: true, force: true }); }
  `;
  const result = spawnSync(process.execPath, ["--experimental-test-module-mocks", "--input-type=module", "-e", script], { encoding: "utf8" });
  assert.equal(result.status, 0, result.stderr);
});
