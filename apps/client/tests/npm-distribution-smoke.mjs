// 通过仅监听回环的临时 registry 安装 npm pack 成品；测试驱动可用 stdin 在 Docker 运行。
import assert from "node:assert/strict";
import { createServer } from "node:http";
import { execFile } from "node:child_process";
import { promisify } from "node:util";
import { mkdtemp, readFile, access, writeFile, realpath } from "node:fs/promises";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { pathToFileURL } from "node:url";
import { createHash } from "node:crypto";
import { DatabaseSync, backup } from "node:sqlite";

const exec = promisify(execFile);
const [artifactsArgument] = process.argv.slice(2);
const artifacts = resolve(artifactsArgument);
const evidence = await realpath(await mkdtemp(join(tmpdir(), "ez-npm-中文 path-")));
const prefix = join(evidence, "prefix"), home = join(evidence, "home");
console.log(`ISOLATED TARGET ${process.platform}/${process.arch} ${home}/data/runtime.sqlite3; no source database; evidence ${evidence}`);
const env = { ...process.env, PATH: `${dirname(process.execPath)}:${process.env.PATH}`, EZ_ASSISTANT_RUNTIME_HOME: home, NO_COLOR: "1", npm_config_userconfig: "/dev/null" };
delete env.EZ_ASSISTANT_RUNTIME_EXECUTABLE; delete env.NODE_OPTIONS;
const report = JSON.parse(await readFile(join(artifacts, "pack-report.json"), "utf8"));
const packages = new Map();
for (const artifact of report.artifacts) {
  const filename = join(artifacts, artifact.filename);
  const metadata = JSON.parse((await exec("tar", ["-xOf", filename, "package/package.json"], { maxBuffer: 1048576 })).stdout);
  packages.set(metadata.name, { metadata, filename, artifact });
}
const registry = createServer(async (request, response) => {
  try {
    const route = decodeURIComponent(new URL(request.url, "http://localhost").pathname).slice(1);
    if (route.startsWith("tarballs/")) {
      const item = [...packages.values()].find((item) => route === `tarballs/${item.artifact.filename}`);
      if (!item) { response.writeHead(404).end(); return; }
      response.end(await readFile(item.filename)); return;
    }
    const item = packages.get(route);
    if (!item) { response.writeHead(404).end('{"error":"fixture package missing"}'); return; }
    response.setHeader("content-type", "application/json");
    response.end(JSON.stringify({ name: route, "dist-tags": { latest: item.metadata.version }, versions: { [item.metadata.version]: { ...item.metadata, dist: { tarball: `${url}/tarballs/${item.artifact.filename}`, integrity: item.artifact.integrity, shasum: item.artifact.shasum } } } }));
  } catch { response.writeHead(500).end(); }
});
await new Promise((accept) => registry.listen(0, "127.0.0.1", accept));
const url = `http://127.0.0.1:${registry.address().port}`;
const npmPath = join(dirname(process.execPath), "../lib/node_modules/npm/bin/npm-cli.js");
const npm = (...args) => exec(process.execPath, [npmPath, ...args, "--prefix", prefix, "--cache", join(evidence, "npm-cache"), "--registry", url, "--ignore-scripts", "--no-audit", "--no-fund"], { env, timeout: 90000, maxBuffer: 1048576 });
const root = join(prefix, "lib/node_modules/@ez-assistant/client");
const load = (path) => import(pathToFileURL(join(root, "dist", path)).href);
const cli = (...args) => exec(join(prefix, "bin/ez-assistant"), ["--runtime-home", home, ...args], { env, timeout: 90000 });
const absent = async (path) => { try { await access(path); return false; } catch (error) { if (error.code === "ENOENT") return true; throw error; } };
const events = [];
let unit;
try {
  await npm("install", "-g", `@ez-assistant/client@${report.clientPackageVersion ?? report.version}`);
  assert.match((await cli("--version")).stdout, /0\.25\.2/);
  await cli("status"); assert(await absent(home));
  events.push("npm global install --ignore-scripts / platform selection / bin / readonly status");
  // 驱动和被测模块使用同一隔离环境，服务的真实 HOME 仍为当前普通用户。
  delete process.env.EZ_ASSISTANT_RUNTIME_EXECUTABLE;
  const { bundledSource } = await load("host/process.js");
  const source = bundledSource();
  assert(source.includes("/node_modules/"));
  const { HostControl } = await load("host/control.js");
  const control = new HostControl(home, new AbortController().signal);
  // port 0 只用于取得空闲测试端口；Host 保存的是真实确定端口。
  const portServer = createServer(); await new Promise((accept) => portServer.listen(0, "127.0.0.1", accept));
  const port = portServer.address().port; await new Promise((accept) => portServer.close(accept));
  await control.helper("configure", { expected_revision: null, configuration: { port, scheme: "http", remote_enabled: false, server_names: [], tls_certificate: null, tls_private_key: null } });
  assert(await absent(join(home, "data")));
  if (process.platform === "linux") {
    const { prepareService, saveService } = await load("platform/systemd/commit.js");
    const draft = await prepareService(home, true);
    assert.equal(draft.source.executable, source);
    unit = (await saveService(draft, new AbortController().signal)).unit;
    events.push("systemd preview and unit use npm Host / enable without start");
  }
  await cli("start");
  const before = await control.status(); assert.equal(before.discovery.executable_path, source);
  assert.equal(before.health.status, "ready");
  const inventoryBefore = await audit("before");
  // 包管理前显式关闭自启并停止；不再承诺卸载后程序或在途进程继续可用。
  if (unit) {
    const { prepareService, saveService } = await load("platform/systemd/commit.js");
    await saveService(await prepareService(home, false), new AbortController().signal);
  }
  await cli("stop");
  await npm("install", "-g", "--force", packages.get("@ez-assistant/client").filename);
  await cli("start");
  assert.equal((await control.status()).discovery.executable_path, source);
  await cli("restart"); await cli("stop");
  events.push("stop before npm reinstall / start and restart use packaged source");
  await npm("uninstall", "-g", "@ez-assistant/client");
  assert(await absent(root)); assert(await absent(join(prefix, "bin/ez-assistant")));
  assert(await absent(source));
  events.push("npm uninstall removes Client and platform Host / preserves data");
  await npm("install", "-g", `@ez-assistant/client@${report.clientPackageVersion ?? report.version}`);
  await cli("start"); await cli("stop");
  assert.deepEqual(await audit("after"), inventoryBefore);
  events.push("reinstall resumes management / original-source restart / controlled stop / exact database preservation");
  await writeFile(join(evidence, "result.json"), JSON.stringify({ platform: process.platform, arch: process.arch, home, source, unit, events, tables: Object.keys(inventoryBefore).length, rows: Object.values(inventoryBefore).reduce((sum, table) => sum + table.count, 0) }, null, 2));
  console.log(`PASS ${events.length} groups; evidence ${evidence}`);
} finally { await new Promise((accept) => registry.close(accept)); }

async function audit(name) {
  const path = join(home, "data/runtime.sqlite3"), destination = join(evidence, `${name}.sqlite3`);
  const database = new DatabaseSync(path, { readOnly: true });
  try { await backup(database, destination); } finally { database.close(); }
  const inspect = (filename) => {
    const db = new DatabaseSync(filename, { readOnly: true });
    try {
      db.exec("BEGIN");
      assert.equal(Object.values(db.prepare("PRAGMA integrity_check").get())[0], "ok");
      const tables = {};
      for (const { name: table } of db.prepare("SELECT name FROM sqlite_schema WHERE type='table' ORDER BY name").all()) {
        const quoted = '"' + table.replaceAll('"', '""') + '"';
        const count = db.prepare(`SELECT COUNT(*) AS count FROM ${quoted}`).get().count;
        const rows = db.prepare(`SELECT * FROM ${quoted}`).all().map((row) => JSON.stringify(row)).sort();
        tables[table] = { count, sha256: createHash("sha256").update(JSON.stringify(rows)).digest("hex") };
      }
      assert.equal(Object.keys(tables).length, 42); return tables;
    } finally { db.close(); }
  };
  const result = inspect(destination); assert.deepEqual(result, inspect(path));
  await writeFile(join(evidence, `${name}-audit.json`), JSON.stringify({ database: path, backup: destination, tables: result }, null, 2));
  return result;
}
