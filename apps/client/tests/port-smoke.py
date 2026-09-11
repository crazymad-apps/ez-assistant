"""占用端口不建库；释放端口后两个真实 Client 共用唯一 Host。"""
from support import Fixture, ROOT, NODE
import json, socket, subprocess

f = Fixture('port-race')
f.seed()
port = f.helper('read')['configuration']['port']
with socket.socket() as occupied:
    occupied.bind(('127.0.0.1', port))
    occupied.listen()
    assert '已被占用' in f.cli('start', expected=1)
    assert not (f.home/'data').exists()
    assert not (f.home/'run/runtime.json').exists()
commands = [subprocess.Popen([NODE, '--use-system-ca', str(ROOT/'dist/cli.js'), 'start'],
    env={**f.env, 'NO_COLOR': '1'}, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True) for _ in range(2)]
outputs = []
for command in commands:
    out, err = command.communicate(timeout=100)
    outputs.append({'code': command.returncode, 'output': out+err})
(f.root/'concurrent-start.json').write_text(json.dumps(outputs, ensure_ascii=False, indent=2))
assert all(item['code'] == 0 for item in outputs), outputs
discovery = f.discovery()
assert all(discovery['address'] in item['output'] for item in outputs)
processes = subprocess.check_output(['ps', '-axo', 'pid=,command='], text=True).splitlines()
assert len([line for line in processes if str(f.source)+' serve ' in line]) == 1
before = f.audit('before')
assert '已复用现有 Host' in f.cli('start')
f.cli('status')
f.cli('stop')
assert not f.helper('probe')['lock_held']
assert before == f.audit('after')
print('PASS occupied port creates no database; simultaneous Clients share one Host; all table fields preserved', flush=True)
