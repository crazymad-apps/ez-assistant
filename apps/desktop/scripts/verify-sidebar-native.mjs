// macOS WKWebView + 正式 Host；每次只使用新建、无用户数据的隔离 Runtime Home。
import { execFile } from 'node:child_process';
import { mkdtemp } from 'node:fs/promises';
import { resolve, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { promisify } from 'node:util';
import { compatibilityHeaders } from '@ez-assistant/protocol';
import setup from '../tests/e2e/runtime-host.setup.ts';

if (process.platform !== 'darwin') throw new Error('此探针只验证 macOS WKWebView');
process.chdir(fileURLToPath(new URL('../', import.meta.url)));
const scene = await mkdtemp(resolve('../../.runtime-test/f04-m2-native-'));
process.env.EZ_ASSISTANT_E2E_TEST_ROOT = scene;
process.env.C03_E2E_RETAIN_HOME = '1';
const cleanup = await setup({});
const runtime_home = process.env.EZ_ASSISTANT_E2E_RUNTIME_HOME;
const host = JSON.parse(process.env.EZ_ASSISTANT_E2E_BOOTSTRAP);
const headers = { ...compatibilityHeaders(), Authorization: `Bearer ${host.access_token}`, 'Content-Type': 'application/json' };
async function command(type, payload = {}) {
  const response = await fetch(`${host.base_url}/commands`, { method: 'POST', headers, body: JSON.stringify({ request_id: crypto.randomUUID(), command: { scope: 'runtime', payload: { type, payload } } }) });
  const body = await response.json();
  if (!response.ok) throw new Error(JSON.stringify(body));
  return body.result.payload.payload;
}
try {
  const source = (await command('create_session', { title: 'Native A' })).session.session_id;
  const reference = (await command('create_session', { title: 'Native B' })).session.session_id;
  const login = await fetch(`${host.base_url}/auth/login`, { method: 'POST', headers, body: JSON.stringify({ method: 'desktop' }) });
  const { token } = await login.json();
  const binary = join(scene, 'sidebar-probe');
  await promisify(execFile)('swiftc', ['tests/native/sidebar-order.swift', '-o', binary], { timeout: 60_000 });
  const result = await promisify(execFile)(binary, [`${host.base_url}/#token=${encodeURIComponent(token)}`, source, reference], { timeout: 60_000 });
  console.log(result.stdout.trim());
  const snapshot = (await command('get_application_snapshot')).snapshot.value;
  const group = snapshot.session_groups.find(group => group.group.type === 'unbound');
  if (group.sessions[0].session_id !== source) throw new Error('Host 快照顺序未持久提交');
  const database = join(runtime_home, 'users/_personal/data/runtime.sqlite3');
  const audit = await promisify(execFile)('python3', ['-c', `import sqlite3,json,sys,pathlib
p=pathlib.Path(sys.argv[1])
with sqlite3.connect(p.as_uri()+'?mode=ro',uri=True) as c:
 print(json.dumps({'database':str(p),'counts':{n:c.execute('SELECT COUNT(*) FROM '+n).fetchone()[0] for n in ['workspaces','sessions','session_resources']},'quick_check':c.execute('PRAGMA quick_check').fetchone()[0],'order':c.execute('SELECT session_id FROM session_resources WHERE workspace_id IS NULL ORDER BY sort_order DESC,session_id').fetchall()}))`, database]);
  console.log(audit.stdout.trim());
} catch (error) {
  if (error.stdout) console.error(error.stdout.trim());
  console.error(error.stderr || '原生侧栏验证失败');
  process.exitCode = 1;
} finally { await cleanup(); }
