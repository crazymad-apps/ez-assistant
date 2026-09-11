from support import Fixture,Terminal

def run(name,body,columns=80):
    f=Fixture(name);t=Terminal(f,columns=columns)
    try:body(f,t);print('PASS '+name,flush=True)
    finally:t.close(name)

def password_cancel(f,t):
    t.enter_access();assert not f.home.exists()
    t.choose(0);t.expect('设置访问密码');secret='isolated-client-password';t.send(secret+'\r')
    t.expect('再次输入访问密码');t.send('wrong\r');t.expect('两次输入不一致');t.replace(secret)
    t.expect('保存新密码后');t.send('\r');t.expect('密码已保存');t.expect('选择要配置的项目')
    t.send('\x03');t.expect('此前已保存');t.finish(130)
    assert f.helper('read')['password_configured'] is True
    assert secret not in t.output and 'wrong' not in t.output
    assert not (f.home/'data').exists()

def narrow_discard(f,t):
    t.enter_access();t.choose(3);t.expect('选择访问协议');t.choose(1)
    t.expect('HTTPS 证书路径');t.send('relative\r');t.expect('请输入绝对文件路径');t.replace('/test/中文证书.crt')
    t.expect('HTTPS 私钥路径');t.send('/test/中文私钥.key\r');t.expect('有未保存修改')
    t.choose(5);t.expect('访问设置预览');t.expect('有未保存修改');t.choose(8)
    t.expect('放弃未保存访问设置');t.send('\r');t.expect('有未保存修改')
    t.choose(8);t.expect('放弃未保存访问设置');t.send('\x1b[D\r');t.expect('选择配置范围');t.choose(2);t.finish()
    assert not f.home.exists();assert '中文证书' in t.output

def offline_conflict(f,t):
    t.enter_access();t.choose(2);t.expect('◆  监听端口');t.replace('0');t.expect('请输入 1–65535');t.replace('7349');t.expect('有未保存修改')
    f.seed()
    t.choose(6);t.expect('保存这些访问设置');t.send('\r');t.expect('配置已被其他入口修改');t.expect('配置状态已变化')
    t.choose(6);t.expect('请先重新读取');t.expect('配置状态已变化');t.choose(7)
    t.expect('重新读取会丢弃');t.send('\x1b[D\r');t.expect('已重新读取');t.expect('选择要配置的项目');t.exit_access()
    assert f.helper('read')['configuration']['port']!=7349;assert not (f.home/'data').exists()

def cancel(f,t,key):t.enter_access();t.send(key);t.finish(130);assert not f.home.exists()

run('password-save-then-cancel',password_cancel)
run('narrow-chinese-preview-discard',narrow_discard,48)
run('offline-conflict-no-replay',offline_conflict)
run('escape-cancel',lambda f,t:cancel(f,t,'\x1b'))
run('ctrl-c-cancel',lambda f,t:cancel(f,t,'\x03'))

# 独占现有锁模拟另一启动操作，取消 Client 等待不释放他人的锁，也不创建数据库。
import fcntl
f=Fixture('start-cancel');f.seed()
with (f.home/'run/runtime.lock').open('r+b') as lock:
    fcntl.flock(lock,fcntl.LOCK_EX|fcntl.LOCK_NB)
    t=Terminal(f,['start'])
    try:
        t.expect('正在查找或启动');t.send('\x03');t.finish(130)
        assert not (f.home/'data').exists()
        print('PASS start cancellation preserves external lock and creates no database',flush=True)
    finally:t.close('start-cancel')
