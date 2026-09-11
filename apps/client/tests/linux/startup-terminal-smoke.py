"""在本机 Docker 中通过真实 PTY 操作编译包；只配置自启，不启动业务 Host。"""
import argparse
import errno
import fcntl
import json
import os
import pathlib
import pty
import re
import select
import struct
import subprocess
import tempfile
import termios
import time

parser = argparse.ArgumentParser()
parser.add_argument('--package', required=True)
parser.add_argument('--container', default='ez-assistant-m5-linux')
parser.add_argument('--user', default='ezservicecheck')
parser.add_argument('--uid', default='2001')
args = parser.parse_args()
assert args.package.startswith('/work/ez-assistant-client-')
assert args.user == 'ezservicecheck' and args.uid == '2001'
evidence = pathlib.Path(tempfile.mkdtemp(prefix='ez-m5-startup-pty-')).resolve()
home = f'/home/{args.user}/{evidence.name}/home'
docker = ['docker', '--context', 'desktop-linux', 'exec']
environment = ['-u', args.user, '-e', f'HOME=/home/{args.user}', '-e', 'TERM=xterm-256color',
               '-e', f'XDG_RUNTIME_DIR=/run/user/{args.uid}',
               '-e', f'DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/{args.uid}/bus']
ansi = re.compile(r'\x1b\[[0-?]*[ -/]*[@-~]')


def query():
    script = f"const m=await import('{args.package}/app/platform/systemd/query.js');console.log(JSON.stringify(await m.querySystemd('{home}')));"
    return json.loads(subprocess.check_output(docker + environment + [args.container,
        args.package + '/runtime/node', '--input-type=module', '-e', script], text=True))


class Terminal:
    def __init__(self, name):
        self.name = name
        self.master, slave = pty.openpty()
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack('HHHH', 32, 48, 0, 0))
        self.process = subprocess.Popen(docker + ['-it'] + environment + [args.container,
            args.package + '/bin/ez-assistant', '--runtime-home', home, 'config'],
            stdin=slave, stdout=slave, stderr=slave, start_new_session=True)
        os.close(slave)
        self.data = b''
        self.cursor = 0

    def read(self):
        if select.select([self.master], [], [], .1)[0]:
            try:
                self.data += os.read(self.master, 65536)
            except OSError as error:
                if error.errno != errno.EIO:
                    raise

    def expect(self, text):
        deadline = time.monotonic() + 15
        while time.monotonic() < deadline:
            output = ansi.sub('', self.data.decode(errors='replace'))
            index = output.find(text, self.cursor)
            if index >= 0:
                self.cursor = index + len(text)
                return
            self.read()
        raise AssertionError(f'missing {text!r}: {output[-2000:]}')

    def send(self, text):
        os.write(self.master, text.encode())
        time.sleep(.08)

    def choose(self, index):
        for _ in range(index):
            self.send('\x1b[B')
        self.send('\r')

    def finish(self, code=0):
        deadline = time.monotonic() + 10
        while self.process.poll() is None and time.monotonic() < deadline:
            self.read()
        assert self.process.poll() == code, self.data.decode(errors='replace')[-2000:]

    def close(self):
        if self.process.poll() is None:
            self.send('\x03')
            self.process.wait(timeout=20)
        os.close(self.master)
        (evidence / (self.name + '.txt')).write_text(ansi.sub('', self.data.decode(errors='replace')))


for name, enabled, commit, cancel_after in [
    ('preview-cancel', True, False, True),
    ('enable-save', True, True, False),
    ('disable-save-then-cancel', False, True, True),
]:
    terminal = Terminal(name)
    try:
        terminal.expect('选择配置范围'); terminal.choose(1)
        terminal.expect('启动设置 · 独立保存'); terminal.choose(0 if enabled else 1)
        terminal.expect('启动设置预览'); terminal.expect('保存这些启动设置？')
        if not commit:
            terminal.send('\x03'); terminal.finish(130)
            assert query()['autostart'] == 'unconfigured'
            subprocess.run(docker + environment + [args.container, 'test', '!', '-e', home], check=True)
        else:
            terminal.send('\x1b[D\r'); terminal.expect('启动设置已保存')
            terminal.expect('启动设置 · 独立保存')
            if cancel_after:
                terminal.send('\x03'); terminal.expect('此前已保存'); terminal.finish(130)
            else:
                terminal.choose(4); terminal.expect('选择配置范围'); terminal.choose(2); terminal.finish()
            actual = query()
            assert actual['autostart'] == ('enabled' if enabled else 'disabled')
            assert actual['state']['mainPid'] == 0 and actual['linger'] is True
            subprocess.run(docker + environment + [args.container, 'test', '!', '-e', home + '/data'], check=True)
        print('PASS ' + name, flush=True)
    finally:
        terminal.close()

summary = {'home': home, 'package': args.package, 'columns': 48, 'cases': 3, 'host_started': False, 'state': query()}
(evidence / 'summary.json').write_text(json.dumps(summary, ensure_ascii=False, indent=2))
print(json.dumps({'result': 'PASS', 'evidence': str(evidence), 'home': home}), flush=True)
