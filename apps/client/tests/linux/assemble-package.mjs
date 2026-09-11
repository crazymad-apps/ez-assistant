// M5 包运行验收夹具：消费已构建产物，不在运行容器安装依赖或执行源码。
import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { cp, mkdir, readFile, readdir, writeFile, lstat } from "node:fs/promises";
import { resolve, join, relative } from "node:path";
import { execFileSync } from "node:child_process";

const [repository, host, nodeRoot, dependencies, destination] = process.argv.slice(2).map((value) => resolve(value));
assert(destination && dependencies && nodeRoot && host && repository,
  "usage: node assemble-package.mjs <repo> <built-host> <node-root> <production-node_modules> <new-output>");
assert.equal(process.platform, "linux");
assert.equal(process.arch, "arm64");
const metadata = JSON.parse(await readFile(join(repository, "apps/client/package.json"), "utf8"));
const info = JSON.parse(execFileSync(host, ["--build-info-json"], { encoding: "utf8" }));
assert.equal(info.version, metadata.version);
const node = join(nodeRoot, "bin/node");
assert.equal(execFileSync(node, ["--version"], { encoding: "utf8" }).trim(), "v24.21.0");
await mkdir(destination); // 已存在则拒绝，避免覆盖正在测试的安装来源。
for (const directory of ["bin", "runtime", "host", "app", "LICENSES"])
  await mkdir(join(destination, directory));
await cp(join(repository, "apps/client/dist"), join(destination, "app"), { recursive: true });
await cp(dependencies, join(destination, "app/node_modules"), {
  recursive: true, dereference: true,
  filter: (source) => !relative(dependencies, source).split("/").includes(".bin"),
});
await writeFile(join(destination, "app/package.json"), JSON.stringify({
  name: metadata.name, version: metadata.version, type: "module",
}, null, 2)+"\n");
await cp(host, join(destination, "host/ez-assistant-runtime"));
await cp(node, join(destination, "runtime/node"));
await cp(join(nodeRoot, "LICENSE"), join(destination, "LICENSES/Node.txt"));
await writeFile(join(destination, "bin/ez-assistant"), `#!/bin/sh
set -eu
package_dir=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd -P)
unset NODE_OPTIONS NODE_PATH
exec "$package_dir/runtime/node" --use-system-ca "$package_dir/app/cli.js" "$@"
`, { mode: 0o755 });
const files = {};
async function inventory(directory) {
  for (const name of (await readdir(directory)).sort()) {
    const path = join(directory, name), stat = await lstat(path);
    assert(!stat.isSymbolicLink(), `包内不得依赖外部链接: ${path}`);
    if (stat.isDirectory()) await inventory(path);
    else files[relative(destination, path)] = createHash("sha256").update(await readFile(path)).digest("hex");
  }
}
await inventory(destination);
assert(!files["app/node_modules/tsx/package.json"]);
assert(!files["app/node_modules/typescript/package.json"]);
await writeFile(join(destination, "manifest.json"), JSON.stringify({
  purpose: "v0.25.2 M5 Linux package verification; not a completed release",
  version: info.version, min_compatible_version: info.min_compatible_version,
  platform: "linux", arch: "arm64", node: "24.21.0", files,
}, null, 2)+"\n");
console.log(`Assembled ${destination}: ${Object.keys(files).length} files`);
