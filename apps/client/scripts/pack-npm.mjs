// 只装配已独立构建的 Host；不控制 Host/Web/Desktop 构建，不执行 npm publish。
import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { cp, mkdir, mkdtemp, readFile, readdir, realpath, stat, writeFile, chmod } from "node:fs/promises";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { execFileSync } from "node:child_process";
import { fileURLToPath } from "node:url";

const client = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const repository = resolve(client, "../..");
const [outputArgument, ...hostArguments] = process.argv.slice(2);
assert(outputArgument && hostArguments.length, "usage: node scripts/pack-npm.mjs <new-output> <darwin-arm64|linux-arm64|linux-x64>=<built-host> ...");
assert.match(process.version, /^v24\./, "使用项目锁定的 Node 24 工具链构建");
const output = resolve(outputArgument);
await mkdir(output); // 不覆盖已有候选包，也不修改 npm 全局目录。
const work = await mkdtemp(join(tmpdir(), "ez-client-npm-build-"));
const npm = process.env.npm_execpath || join(dirname(process.execPath), "../lib/node_modules/npm/bin/npm-cli.js");
const runNpm = (args, cwd) => execFileSync(process.execPath, [npm, ...args], { cwd, stdio: ["ignore", "pipe", "pipe"], timeout: 180000 });
runNpm(["run", "check:version"], client);
runNpm(["run", "build"], join(repository, "packages/assistant-protocol"));
const compiled = join(work, "compiled");
execFileSync(process.execPath, [join(client, "node_modules/typescript/bin/tsc"), "-p", join(client, "tsconfig.json"), "--outDir", compiled], { cwd: client, timeout: 30000 });
const metadata = JSON.parse(await readFile(join(client, "package.json"), "utf8"));
const declarations = await import(new URL("../../../packages/assistant-protocol/dist/index.js", import.meta.url));
const { SOFTWARE_VERSION: version, MIN_COMPATIBLE_VERSION: minimum } = declarations;
assert.equal(metadata.version, version);
// npm 包修订与应用兼容版本分离；Host 更新时附件也必须使用新包号。
const revision = process.env.EZ_ASSISTANT_NPM_CLIENT_REVISION;
assert(revision === undefined || /^[1-9][0-9]*$/.test(revision), "Client npm 修订号必须是正整数");
const clientPackageVersion = revision === undefined ? version : `${version}-${revision}`;
const hostRevision = process.env.EZ_ASSISTANT_NPM_HOST_REVISION;
assert(hostRevision === undefined || /^[1-9][0-9]*$/.test(hostRevision), "Host npm 修订号必须是正整数");
const hostPackageVersion = hostRevision === undefined ? version : `${version}-${hostRevision}`;

// 按现有 lock 在独立目录安装生产依赖；开发工作区 node_modules 不能整体进入发行包。
const stagedClient = join(work, "apps/client"), stagedProtocol = join(work, "packages/assistant-protocol");
await mkdir(stagedClient, { recursive: true }); await mkdir(stagedProtocol, { recursive: true });
for (const name of ["package.json", "package-lock.json"]) await cp(join(client, name), join(stagedClient, name));
await cp(join(repository, "packages/assistant-protocol/dist"), join(stagedProtocol, "dist"), { recursive: true });
await writeFile(join(stagedProtocol, "package.json"), JSON.stringify({ name: "@ez-assistant/protocol", version, type: "module", exports: { "./node": "./dist/index.js" } }));
runNpm(["ci", "--omit=dev", "--ignore-scripts", "--no-audit", "--no-fund"], stagedClient);

const artifacts = [], platforms = {};
for (const argument of hostArguments) {
  const separator = argument.indexOf("="), target = argument.slice(0, separator);
  assert(separator > 0 && ["darwin-arm64", "linux-arm64", "linux-x64"].includes(target) && !platforms[target], "平台参数错误或重复");
  const host = await realpath(resolve(argument.slice(separator + 1))), [os, cpu] = target.split("-");
  const bytes = await readFile(host);
  // 不执行跨架构 Host；确认文件头与声明匹配。原生包另核实 build-info。
  if (os === "linux") {
    assert.equal(bytes.subarray(0, 4).toString("hex"), "7f454c46");
    assert.equal(bytes[4], 2); assert.equal(bytes[5], 1);
    assert.equal(bytes.readUInt16LE(18), cpu === "arm64" ? 183 : 62);
  } else { assert.equal(bytes.readUInt32LE(0), 0xfeedfacf); assert.equal(bytes.readUInt32LE(4), 0x0100000c); }
  if (os === process.platform && cpu === process.arch) {
    const info = JSON.parse(execFileSync(host, ["--build-info-json"], { encoding: "utf8", timeout: 5000 }));
    assert.equal(info.version, version); assert.equal(info.min_compatible_version, minimum);
  }
  const name = `@ez-assistant/client-${target}`, directory = join(work, `platform-${target}`);
  await mkdir(join(directory, "host"), { recursive: true });
  await cp(host, join(directory, "host/ez-assistant-runtime")); await chmod(join(directory, "host/ez-assistant-runtime"), 0o755);
  await writeFile(join(directory, "host/manifest.json"), JSON.stringify({ version, min_compatible_version: minimum, platform: os, arch: cpu, sha256: createHash("sha256").update(bytes).digest("hex") }, null, 2) + "\n");
  await writeFile(join(directory, "package.json"), JSON.stringify({ name, version: hostPackageVersion, os: [os], cpu: [cpu], ...(os === "linux" ? { libc: ["glibc"] } : {}), files: ["host"], license: "UNLICENSED", description: "EZ Assistant Client platform payload (Host with embedded Web)" }, null, 2));
  const packed = JSON.parse(runNpm(["pack", "--ignore-scripts", "--json", "--pack-destination", output], directory))[0];
  artifacts.push(packed); platforms[target] = name;
}
const main = join(work, "main"); await mkdir(main);
await cp(compiled, join(main, "dist"), { recursive: true });
await cp(join(stagedClient, "node_modules"), join(main, "node_modules"), { recursive: true, dereference: true, filter: (path) => !path.split("/").includes(".bin") });
await cp(join(client, "README.md"), join(main, "README.md"));
await chmod(join(main, "dist/distribution/npm-entry.js"), 0o755);
const dependencies = { ...metadata.dependencies, "@ez-assistant/protocol": version };
await writeFile(join(main, "package.json"), JSON.stringify({
  name: metadata.name, version: clientPackageVersion, description: "EZ Assistant · 本机 Host 管理", type: "module", license: "UNLICENSED",
  engines: metadata.engines, os: ["darwin", "linux"], cpu: ["arm64", "x64"],
  ezAssistantDistribution: "npm", bin: { "ez-assistant": "dist/distribution/npm-entry.js" },
  files: ["dist"], dependencies, bundledDependencies: Object.keys(dependencies),
  optionalDependencies: Object.fromEntries(Object.values(platforms).map((name) => [name, hostPackageVersion])),
}, null, 2) + "\n");
artifacts.push(JSON.parse(runNpm(["pack", "--ignore-scripts", "--json", "--pack-destination", output], main))[0]);
for (const artifact of artifacts) {
  for (const file of artifact.files) assert(!/^(src|tests)\//.test(file.path) && !/node_modules\/(tsx|typescript)\//.test(file.path) && !/node_modules\/@ez-assistant\/protocol\/src\//.test(file.path), `包内出现源码／测试／开发依赖：${file.path}`);
}
await writeFile(join(output, "pack-report.json"), JSON.stringify({ version, clientPackageVersion, hostPackageVersion, candidates: Object.keys(platforms), artifacts, work, releaseReady: false, note: "候选包；平台运行、许可证审计、签名和发布权限需要独立证据" }, null, 2) + "\n");
console.log(`npm 候选包已生成：${output}（${artifacts.length} 包；未发布）`);
