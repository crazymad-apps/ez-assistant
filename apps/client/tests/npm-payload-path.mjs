// 成品包路径验证；拦截 launch，不运行安装、业务 Host 或数据库，不保存系统服务。
import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { mkdtemp, mkdir, chmod, readFile, realpath } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { pathToFileURL } from "node:url";

const packages = resolve(process.argv[2]);
const report = JSON.parse(await readFile(join(packages, "pack-report.json"), "utf8"));
const root = await realpath(await mkdtemp(join(tmpdir(), "ez-npm-payload-path-")));
const mainName = "@ez-assistant/client", platformName = `${mainName}-${process.platform}-${process.arch}`;
for (const name of [mainName, platformName]) {
  const artifact = report.artifacts.find((item) => item.name === name);
  assert(artifact, `缺少测试平台 ${name}`);
  const destination = join(root, "node_modules", name);
  await mkdir(destination, { recursive: true });
  execFileSync("tar", ["-xzf", join(packages, artifact.filename), "--strip-components=1", "-C", destination]);
}
const home = join(root, "home"), main = join(root, "node_modules", mainName);
await mkdir(home, { mode: 0o750 });
await mkdir(join(home, ".local")); await chmod(join(home, ".local"), 0o775);
const env = { ...process.env, HOME: home };
delete env.EZ_ASSISTANT_RUNTIME_EXECUTABLE;
delete env.EZ_ASSISTANT_RUNTIME_HOME;
const moduleUrl = (name) => JSON.stringify(pathToFileURL(join(main, "dist", name)).href);
const script = `
  import assert from 'node:assert/strict';
  import { mock } from 'node:test';
  import { access, stat, readFile } from 'node:fs/promises';
  import { createServer } from 'node:net';
  import { join } from 'node:path';
  const processModule = await import(${moduleUrl("host/process.js")});
  const source = processModule.bundledSource();
  assert.equal(source, ${JSON.stringify(join(root, "node_modules", platformName, "host/ez-assistant-runtime"))});
  assert.equal((await processModule.buildInfo(source)).version, ${JSON.stringify(report.version)});
  const launched = [];
  mock.module(${moduleUrl("host/process.js")}, { namedExports: { ...processModule,
    localProcess: async (file, args) => { launched.push({ file, args }); return { code: 0 }; }
  }});
  const { HostControl } = await import(${moduleUrl("host/control.js")});
  const runtimeHome = join(process.env.HOME, 'runtime');
  const server = createServer(); await new Promise(r => server.listen(0, '127.0.0.1', r));
  const port = server.address().port; await new Promise(r => server.close(r));
  const control = new HostControl(runtimeHome, new AbortController().signal);
  control.target = async () => null; control.locked = async () => false;
  control.helper = async () => ({ revision: null, password_configured: false,
    configuration: { port, scheme: 'http', remote_enabled: false, server_names: [], tls_certificate: null, tls_private_key: null } });
  control.waitReady = async () => ({ health: { status: 'ready' } });
  await control.start();
  assert.deepEqual(launched, [{ file: source, args: ['launch', '--runtime-home', runtimeHome] }]);
  if (process.platform === 'linux') {
    const { prepareService } = await import(${moduleUrl("platform/systemd/commit.js")});
    const { renderUnit, parseUnit } = await import(${moduleUrl("platform/systemd/unit.js")});
    const draft = await prepareService(runtimeHome, true);
    assert.equal(draft.source.executable, source);
    assert.deepEqual(parseUnit(renderUnit(draft.source), runtimeHome, draft.source.userHome), draft.source);
  }
  assert.equal((await stat(join(process.env.HOME, '.local'))).mode & 0o777, 0o775);
  for (const path of [runtimeHome, join(process.env.HOME, '.ez-assistant-client'),
    join(process.env.HOME, '.local/share/ez-assistant/client-hosts'), ${JSON.stringify(join(main, "dist/distribution/retained-host.js"))}]) {
    await assert.rejects(access(path), { code: 'ENOENT' });
  }
  const metadata = JSON.parse(await readFile(${JSON.stringify(join(main, "package.json"))}, 'utf8'));
  assert.equal(metadata.version, ${JSON.stringify(report.clientPackageVersion)});
  assert.equal(metadata.optionalDependencies[${JSON.stringify(platformName)}], ${JSON.stringify(report.hostPackageVersion ?? report.version)});
  console.log('PASS npm source / manifest / start uses packaged executable (launch intercepted) / service preview / no external copy or chmod');
`;
execFileSync(process.execPath, ["--experimental-test-module-mocks", "--input-type=module", "-e", script], { env, stdio: "inherit", timeout: 30000 });
console.log(`证据目录：${root}`);
