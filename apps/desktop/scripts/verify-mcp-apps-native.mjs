// 正式 Host + 共用容器 + 原生 Tauri CSP；所有测试数据归新建 .runtime-test 场景。
import { spawn, execFile } from 'node:child_process';
import { mkdtemp, readFile, writeFile } from 'node:fs/promises';
import { join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { promisify } from 'node:util';
import { build } from 'vite';
import { compatibilityHeaders } from '@ez-assistant/protocol';
import setup from '../tests/e2e/runtime-host.setup.ts';

const desktop = fileURLToPath(new URL('../', import.meta.url));
process.chdir(desktop);
const scene = await mkdtemp(resolve('../../.runtime-test/v0330-f03-m0-native-'));
const { version } = JSON.parse(await readFile('package.json', 'utf8'));
process.env.EZ_ASSISTANT_E2E_TEST_ROOT = scene;
process.env.EZ_ASSISTANT_E2E_MCP_APPS = '1';
process.env.C03_E2E_RETAIN_HOME = '1';
const cleanup = await setup({});
// cleanup 会释放环境变量；核验始终使用本次已冻结的路径。
const runtime_home = process.env.EZ_ASSISTANT_E2E_RUNTIME_HOME;
const host = JSON.parse(process.env.EZ_ASSISTANT_E2E_BOOTSTRAP);

async function counts() {
  const query = `import json,sqlite3,sys
from pathlib import Path
p=Path(sys.argv[1])
with sqlite3.connect(p.as_uri()+'?mode=ro',uri=True) as c:
 names=[r[0] for r in c.execute("SELECT name FROM sqlite_schema WHERE type='table' ORDER BY name")]
 print(json.dumps({n:c.execute('SELECT COUNT(*) FROM "'+n.replace('"','""')+'"').fetchone()[0] for n in names}))`;
  const database = join(runtime_home, 'users/_personal/data/runtime.sqlite3');
  const result = await promisify(execFile)(process.platform === 'win32' ? 'python' : 'python3', [
    '-c',
    query,
    database,
  ]);
  return JSON.parse(result.stdout);
}

const before = await counts();
let exit_code = 1;
try {
  let resource;
  const deadline = Date.now() + 10_000;
  while (Date.now() < deadline) {
    const response = await fetch(`${host.base_url}/commands`, {
      method: 'POST',
      headers: {
        ...compatibilityHeaders(),
        Authorization: `Bearer ${host.access_token}`,
        'Content-Type': 'application/json',
      },
      body: JSON.stringify({
        request_id: 'native-ui-resource',
        command: {
          scope: 'runtime',
          payload: {
            type: 'read_mcp_ui_resource',
            payload: { target: { source: { type: 'user' }, server_key: 'ui_fixture' }, uri: 'ui://fixture/card' },
          },
        },
      }),
    });
    if (response.ok) {
      resource = (await response.json()).result.payload.payload.resource;
      break;
    }
    await new Promise((done) => setTimeout(done, 50));
  }
  if (!resource) throw new Error('通用 MCP UI 资源不可用，请先构建当前 Host。');
  await writeFile(
    join(scene, 'resource.json'),
    JSON.stringify({ kind: 'resource', resource, proxy_url: `${host.base_url}/ui/sandbox-proxy` }),
  );
  await writeFile(
    join(scene, 'probe.html'),
    '<!doctype html><meta charset=utf-8><link rel=stylesheet href=style.css><div id=root></div><script type=module src=probe.js></script>',
  );
  await build({
    configFile: false,
    logLevel: 'error',
    define: { __APP_VERSION__: JSON.stringify(version), 'process.env.NODE_ENV': JSON.stringify('production') },
    build: {
      outDir: scene,
      emptyOutDir: false,
      lib: {
        entry: resolve('tests/native/mcp-apps.tsx'),
        formats: ['es'],
        fileName: () => 'probe.js',
        cssFileName: 'style',
      },
    },
  });
  const executable =
    process.env.EZ_ASSISTANT_MCP_APPS_PROBE_BINARY ??
    resolve(`../../target/debug/examples/mcp_apps_probe${process.platform === 'win32' ? '.exe' : ''}`);
  exit_code = await new Promise((done, reject) => {
    const child = spawn(executable, [scene], { stdio: ['ignore', 'pipe', 'pipe'] });
    const output = { stdout: '', stderr: '' };
    let output_bytes = 0;
    let output_overflow = false;
    for (const name of ['stdout', 'stderr']) {
      child[name].on('data', (chunk) => {
        process[name].write(chunk);
        output_bytes += chunk.length;
        if (output_bytes <= 256 * 1024) output[name] += chunk.toString();
        else output_overflow = true;
      });
    }
    child.once('error', reject);
    // Windows 原生应用可能在输出 FAIL 后仍返回 0；等管道关闭后核对明确的成功回执。
    child.once('close', (code) => {
      const pass_count = output.stdout.match(/^PASS: /gm)?.length ?? 0;
      const failed = /^FAIL:/m.test(`${output.stdout}\n${output.stderr}`);
      const passed = code === 0 && pass_count === 1 && !failed && !output_overflow;
      if (!passed) console.error('原生探针未提供唯一成功回执，或报告失败；本次验证不通过。');
      done(passed ? 0 : code || 1);
    });
  });
} finally {
  await cleanup();
  const after = await counts();
  const unchanged = JSON.stringify(before) === JSON.stringify(after);
  console.log(
    JSON.stringify({
      runtime_home,
      database: 'local SQLite; no DB port',
      exact_counts_unchanged: unchanged,
      counts: after,
    }),
  );
  if (!unchanged) {
    console.error('隔离库核验不符，停止后续操作并保留现场。');
    exit_code = 1;
  }
}
process.exitCode = exit_code;
