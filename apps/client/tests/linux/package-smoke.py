"""从宿主驱动干净 Docker 容器中的二进制包；不复制源码或安装测试依赖。"""
import argparse, hashlib, json, pathlib, shlex, sqlite3, subprocess, tempfile, time

parser = argparse.ArgumentParser()
parser.add_argument('--container', default='ez-assistant-m5-linux')
parser.add_argument('--package', required=True, help='容器内已解压的包目录')
args = parser.parse_args()
docker = ['docker', '--context', 'desktop-linux']
container = args.container
package = pathlib.PurePosixPath(args.package)
assert package.is_absolute() and str(package).startswith('/work/')
evidence = pathlib.Path(tempfile.mkdtemp(prefix='ez-linux-package-')).resolve()
home = f'/home/eztest/package-tests/{evidence.name}/home'
entry = str(package/'bin/ez-assistant')
node = str(package/'runtime/node')
host = str(package/'host/ez-assistant-runtime')
events = []

def call(command, data=None, timeout=100):
    result = subprocess.run(command, input=data, capture_output=True, text=True, timeout=timeout)
    assert result.returncode == 0, f'{command[:4]}: {result.stderr or result.stdout}'
    return result.stdout

def run(*command, root=False, data=None):
    prefix = ['exec', '-i']
    if not root:
        prefix += ['-u', 'eztest', '-e', 'HOME=/home/eztest',
                   '-e', 'XDG_RUNTIME_DIR=/run/user/2000',
                   '-e', 'DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/2000/bus']
    return call(docker+prefix+[container, *command], data)

def cli(*command):
    output = run('env', '-u', 'EZ_ASSISTANT_RUNTIME_EXECUTABLE', '-u', 'EZ_ASSISTANT_RUNTIME_HOME',
                 'NODE_OPTIONS=--import=/missing-should-be-ignored', 'NO_COLOR=1',
                 entry, '--runtime-home', home, *command)
    events.append({'command': list(command), 'output': output})
    (evidence/'commands.json').write_text(json.dumps(events, ensure_ascii=False, indent=2))
    return output

def discovery():
    # 凭据仅留在驱动器内存，不写入验收日志。
    return json.loads(run('cat', home+'/run/runtime.json'))

def health():
    d = discovery()
    port = d['address'].rsplit(':', 1)[1]
    assert port.isdigit()
    request = (f'GET /health HTTP/1.0\r\nHost: 127.0.0.1:{port}\r\n'
               f'Authorization: Bearer {d["access_token"]}\r\n'
               'X-Ez-Client-Version: 0.25.2\r\nX-Ez-Min-Compatible-Version: 0.25.2\r\n\r\n')
    response = run('bash', '-s', data=f'exec 3<>/dev/tcp/127.0.0.1/{port}\nprintf %s {shlex.quote(request)} >&3\ncat <&3\n')
    assert response.startswith('HTTP/1.0 200') or response.startswith('HTTP/1.1 200')
    return json.loads(response.split('\n\n', 1)[1])

def wait_ready():
    for _ in range(60):
        try:
            if health()['status'] == 'ready': return discovery()
        except (AssertionError, ValueError, KeyError): pass
        time.sleep(1)
    raise AssertionError('Host not ready; preserve container and evidence')

def ctl(*command): return run('systemctl', '--user', '--no-pager', *command).strip()
def property_(field): return ctl('show', unit, f'--property={field}', '--value')

def audit(name):
    # 仅在确认 Host 已停止后复制实际 SQLite；不读取缓存、估算行数或恢复源库。
    run('test', '!', '-e', home+'/run/runtime.json')
    probe = json.loads(run(host, 'access', 'probe', '--runtime-home', home))
    assert probe['lock_held'] is False
    copied = evidence/(name+'-source.sqlite3')
    call(docker+['cp', container+':'+home+'/data/runtime.sqlite3', str(copied)])
    def inventory(path):
        with sqlite3.connect(path.as_uri()+'?mode=ro', uri=True) as db:
            db.execute('BEGIN')
            assert db.execute('PRAGMA integrity_check').fetchall() == [('ok',)]
            assert db.execute('SELECT * FROM database_compatibility').fetchall() == [(1, '0.25.2')]
            tables = {}
            for (name_,) in db.execute("SELECT name FROM sqlite_schema WHERE type='table' ORDER BY name").fetchall():
                quoted = '"'+name_.replace('"', '""')+'"'
                count = db.execute('SELECT COUNT(*) FROM '+quoted).fetchone()[0]
                rows = sorted(repr(row) for row in db.execute('SELECT * FROM '+quoted))
                tables[name_] = {'count': count, 'fields_sha256': hashlib.sha256('\n'.join(rows).encode()).hexdigest()}
            assert len(tables) == 42
            return tables
    before = inventory(copied)
    backup = evidence/(name+'-backup.sqlite3')
    with sqlite3.connect(copied.as_uri()+'?mode=ro', uri=True) as source, sqlite3.connect(backup) as target:
        source.backup(target)
    assert inventory(backup) == before
    (evidence/(name+'-audit.json')).write_text(json.dumps({
        'host': container, 'port': 'SQLite file; no database network listener',
        'database': home+'/data/runtime.sqlite3', 'snapshot': str(copied),
        'backup': str(backup), 'tables': before,
    }, indent=2))
    return before

info = json.loads(call(docker+['inspect', container]))[0]
assert info['Config']['Labels']['com.ez-assistant.purpose'] == 'm5-package-test'
assert not info['Mounts'] and not info['HostConfig']['PortBindings']
run('sh', '-c', 'for tool in node npm cargo rustc gcc; do if command -v "$tool"; then exit 1; fi; done')
run('test', '!', '-e', '/work/ez-assistant')
run('test', '!', '-e', home)
print(f'ISOLATED TARGET {container}:{home}/data/runtime.sqlite3; evidence {evidence}', flush=True)
print('No source database. First start initializes a new database; all later checks compare 42 tables.', flush=True)

verify = """const fs=require('node:fs'),p=require('node:path'),crypto=require('node:crypto');
const root=process.argv[1],m=JSON.parse(fs.readFileSync(p.join(root,'manifest.json')));
for(const [file,hash] of Object.entries(m.files)){
if(crypto.createHash('sha256').update(fs.readFileSync(p.join(root,file))).digest('hex')!==hash)throw Error(file);
}console.log(JSON.stringify({version:m.version,node:process.version,arch:process.arch,files:Object.keys(m.files).length}));"""
manifest_check = json.loads(run(node, '-e', verify, str(package)))
assert manifest_check['version'] == '0.25.2' and manifest_check['arch'] == 'arm64'
assert '0.25.2' in cli('--version')
assert '未启动' in cli('status')
configured = json.loads(run(host, 'access', 'configure', '--runtime-home', home, data=json.dumps({
    'expected_revision': None, 'configuration': {'port': 17240, 'scheme': 'http',
    'remote_enabled': False, 'server_names': [], 'tls_certificate': None, 'tls_private_key': None},
})))
run(host, 'access', 'set-password', '--runtime-home', home, data=json.dumps({
    'expected_revision': configured['revision'], 'password': 'isolated-package-test-password',
}))
cli('start'); original = wait_ready()
assert original['executable_path'] == host
assert '已复用现有 Host' in cli('start')
assert discovery()['instance_id'] == original['instance_id']
# 验证内嵌 Web 由 Host 提供；不开浏览器、不启动 Desktop。
web = run(node, '--input-type=module', '-e',
          "const r=await fetch('http://127.0.0.1:17240/');const t=await r.text();if(r.status!==200||!t.includes('<html'))throw Error('Web unavailable');console.log(r.status)")
assert web.strip() == '200'
cli('stop'); baseline = audit('before-lifecycle')
cli('start'); before_restart = discovery()['instance_id']; cli('restart')
assert discovery()['instance_id'] != before_restart
assert discovery()['executable_path'] == host
cli('stop'); assert audit('after-lifecycle') == baseline
print('PASS package help / default source / start / reuse / restart / stop / embedded Web', flush=True)

# 平台前置实测：直接写入产品模板并调用 systemd；不是尚未交付的 config 自启流程。
render = """import {mkdir,writeFile} from 'node:fs/promises';
const [pkg,home]=process.argv.slice(1);
const {unitName,renderUnit}=await import(pkg+'/app/platform/systemd/unit.js');
const directory='/home/eztest/.config/systemd/user';await mkdir(directory,{recursive:true,mode:0o700});
const name=unitName(home);await writeFile(directory+'/'+name,renderUnit({runtimeHome:home,executable:pkg+'/host/ez-assistant-runtime',userHome:'/home/eztest'}),{mode:0o600,flag:'wx'});console.log(name);"""
unit = run(node, '--input-type=module', '-e', render, str(package), home).strip()
run('loginctl', 'enable-linger', '2000', root=True)
ctl('daemon-reload'); ctl('enable', unit)
assert property_('MainPID') == '0' and property_('UnitFileState') == 'enabled'
assert '已开启' in cli('status')
ctl('start', unit); first_service = wait_ready()
assert str(first_service['pid']) == property_('MainPID')
ctl('disable', unit)
assert property_('UnitFileState') == 'disabled' and discovery()['instance_id'] == first_service['instance_id']
assert health()['status'] == 'ready'
ctl('enable', unit); cli('stop')
assert property_('MainPID') == '0' and property_('UnitFileState') == 'enabled'
assert audit('before-container-boot') == baseline

# 启动已停止的测试实例：不带 Client 入口和可执行 Node，验证 unit 直接依赖 Host。
run('chmod', '000', node, entry)
call(docker+['restart', '--timeout', '40', container], timeout=60)
booted = wait_ready()
assert booted['executable_path'] == host and str(booted['pid']) == property_('MainPID')
assert run('ps', '-o', 'uid=', '-p', str(booted['pid'])).strip() == '2000'
assert run('readlink', f'/proc/{booted["pid"]}/exe').strip() == host
run('chmod', '755', node, entry)
assert '已开启' in cli('status')
ctl('disable', unit); cli('stop')
assert property_('MainPID') == '0' and property_('UnitFileState') == 'disabled'
assert audit('after-container-boot') == baseline
summary = {'container': container, 'package': str(package), 'runtime_home': home, 'unit': unit,
           'manifest': manifest_check, 'tables': len(baseline), 'business_row_delta': 0,
           'baseline_total_rows': sum(row['count'] for row in baseline.values()),
           'backups': 4, 'host_stopped': True, 'unit_enabled': False, 'linger': True,
           'scope': 'Client + Host package and real systemd; container boot is not whole-machine A21'}
(evidence/'summary.json').write_text(json.dumps(summary, indent=2))
print('PASS real unit enable/disable independence; boot without executable Client/Node; UID/source/health; exact database preservation', flush=True)
print(json.dumps(summary, ensure_ascii=False), flush=True)
