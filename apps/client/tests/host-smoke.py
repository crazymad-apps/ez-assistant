"""真实 Node Client 与 Host 的本机闭环；每项源库和独立备份保留于临时目录。"""
from support import Fixture,Terminal,inventory,ROOT,NODE
import json,os,subprocess,time

def main():
    f=Fixture('lifecycle');f.seed(password=True)
    assert '未启动' in f.cli('status')
    f.cli('start');first=f.discovery();before=f.audit('before')
    assert '已复用现有 Host' in f.cli('start');assert f.discovery()['instance_id']==first['instance_id']
    assert '手动访问' in f.cli('web');assert 'token=' not in f.logs[-1]['output'];assert first['access_token'] not in json.dumps(f.logs)
    f.cli('restart');second=f.discovery();assert first['instance_id']!=second['instance_id'];assert first['executable_path']==second['executable_path'];assert before==inventory(f.home/'data/runtime.sqlite3')
    # 在线端口修改经真实交互保存；当前监听保留，重启后才切换。
    import socket
    with socket.socket() as sock:sock.bind(('127.0.0.1',0));new_port=sock.getsockname()[1]
    t=Terminal(f)
    try:
        t.enter_access();t.choose(2);t.expect('◆  监听端口');t.replace(str(new_port));t.expect('有未保存修改');t.choose(6);t.expect('保存这些访问设置');t.send('\r');t.expect('需显式 restart');t.expect('选择要配置的项目');t.exit_access()
    finally:t.close('online-port-save')
    assert f.discovery()['address']==second['address'];assert '待重启设置' in f.cli('status')
    f.cli('restart');assert f.discovery()['address'].endswith(':'+str(new_port))
    f.cli('stop');assert not (f.home/'run/runtime.json').exists();assert before==f.audit('after')
    print('PASS lifecycle / online config / web handoff / exact table preservation',flush=True)

    g=Fixture('offline-to-online-conflict');g.seed()
    t=Terminal(g)
    try:
        t.enter_access();t.choose(2);t.expect('◆  监听端口');t.replace('7349');t.expect('有未保存修改')
        g.cli('start');before=g.audit('before')
        t.choose(6);t.expect('保存这些访问设置');t.send('\r');t.expect('正在持锁');t.expect('配置状态已变化')
        t.choose(7);t.expect('重新读取会丢弃');t.send('\x1b[D\r');t.expect('已重新读取');t.expect('选择要配置的项目');t.exit_access()
        assert g.access('get_status')['configuration']['port']!=7349
    finally:t.close('offline-to-online')
    g.cli('stop');assert before==g.audit('after')
    print('PASS offline editor cannot replay online; all table fields unchanged',flush=True)

    h=Fixture('coexist');h.seed()
    # 模拟 Desktop 直接调用其持有的 Host 启动器，Client 以另一随包副本管理同一 Home。
    import shutil
    desktop_source=h.root/'desktop-host';shutil.copy2(h.source,desktop_source)
    result=subprocess.run([str(desktop_source),'launch','--runtime-home',str(h.home)],env=h.env,capture_output=True,timeout=5);assert result.returncode==0
    assert '已复用现有 Host' in h.cli('start');before=h.audit('before');original=h.discovery();assert original['executable_path']==str(desktop_source)
    h.cli('restart');assert h.discovery()['executable_path']==str(desktop_source)
    desktop_source.rename(h.root/'desktop-host-retained')
    assert '原 Host 来源缺失' in h.cli('restart',expected=1);assert h.request('/health')['status']=='ready'
    h.cli('stop');assert before==h.audit('after')
    print('PASS Desktop-owned source retained; missing source rejected; Client bundle can stop',flush=True)

if __name__=='__main__':main()
