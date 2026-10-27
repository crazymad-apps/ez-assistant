import assert from "node:assert/strict";
import { mkdtemp, readFile, writeFile, rm, stat } from "node:fs/promises";
import { join } from "node:path";
import { tmpdir } from "node:os";
import { spawnSync } from "node:child_process";
import { test } from "node:test";
import { installationScript } from "../../scripts/package-macos.mjs";

// 在临时脚本目录使用无副作用替身，只观察退出码与参数；不执行安装或接触 /Applications。
test("macOS PKG scripts preserve helper failures, quote product names, and retain the original user", { skip: process.platform !== "darwin" }, async () => {
  const root = await mkdtemp(join(tmpdir(), "ez-pkg-script-"));
  try {
    await writeFile(join(root, "install-helper"), '#!/bin/sh\nprintf \'%s\\n\' "$@" > "$(dirname "$0")/arguments"\nexit "${TEST_HELPER_STATUS:-0}"\n', { mode: 0o755 });
    const pre = join(root, "preinstall"); const post = join(root, "postinstall");
    const product = "EZ Assistant's $(touch SHOULD_NOT_EXIST)";
    await writeFile(pre, installationScript(product), { mode: 0o755 });
    await writeFile(post, installationScript(product, true), { mode: 0o755 });
    const run = (file, volume = "/", status = 0) => spawnSync("/bin/sh", [file, "package.pkg", "/Applications", volume], { cwd: root, env: { ...process.env, TEST_HELPER_STATUS: String(status) }, encoding: "utf8" });
    assert.equal(run(pre, "/Volumes/Other").status, 1);
    assert.equal(run(pre, "/", 37).status, 37);
    assert.match(await readFile(join(root, "arguments"), "utf8"), /--prepare-installation\n\/Applications\/EZ Assistant's \$\(touch SHOULD_NOT_EXIST\)\.app\n\d+\n/);
    assert.equal(await stat(join(root, "SHOULD_NOT_EXIST")).then(() => true, () => false), false);
    assert.equal((await stat(join(root, "install-user"))).mode & 0o777, 0o600);
    const user = (await readFile(join(root, "install-user"), "utf8")).trim();
    assert.equal(run(post).status, 0);
    const args = (await readFile(join(root, "arguments"), "utf8")).split("\n");
    assert.equal(args[0], "--finish-installation"); assert.equal(args[2], user);
    assert.equal(run(post, "/", 19).status, 19);
  } finally { await rm(root, { recursive: true, force: true }); }
});
