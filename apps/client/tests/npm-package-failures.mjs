// 对已隔离安装的候选包做损坏／缺失与并发验证，不启动业务 Host，不操作数据库。
import assert from "node:assert/strict";
import { readFile, writeFile, rename, realpath } from "node:fs/promises";
import { join, dirname, resolve } from "node:path";
import { pathToFileURL } from "node:url";
import { execFileSync } from "node:child_process";

const root = await realpath(resolve(process.argv[2]));
assert(root.includes("ez-npm-"), "仅允许 smoke 创建的隔离安装");
delete process.env.EZ_ASSISTANT_RUNTIME_EXECUTABLE;
const { bundledSource, buildInfo } = await import(pathToFileURL(join(root, "dist/host/process.js")));
const source = bundledSource(), platform = resolve(source, "../.."), manifest = join(dirname(source), "manifest.json");
await rename(platform, `${platform}.missing`);
try { assert.throws(() => bundledSource(), /缺少匹配的平台包/); }
finally { await rename(`${platform}.missing`, platform); }
const packagePath = join(platform, "package.json"), originalPackage = await readFile(packagePath);
await writeFile(packagePath, JSON.stringify({ ...JSON.parse(originalPackage), version: "0.0.0" }));
try { assert.throws(() => bundledSource(), /缺少匹配的平台包/); }
finally { await writeFile(packagePath, originalPackage); }
const original = await readFile(manifest);
await writeFile(manifest, JSON.stringify({ ...JSON.parse(original), sha256: "0".repeat(64) }));
try { await assert.rejects(buildInfo(source), /平台包内容与清单不一致/); }
finally { await writeFile(manifest, original); }
if (process.argv[3]) {
  assert.throws(() => execFileSync(process.argv[3], [join(root, "dist/distribution/npm-entry.js"), "--help"], { encoding: "utf8", stdio: "pipe" }), (error) => error.status === 1 && /需要 Node/.test(error.stderr));
}
console.log(`PASS missing payload / digest mismatch before execution / optional old Node; ${root}`);
