"""复用现有 Desktop 原生重启验收函数，验证 Client 先启动时的真实双产品方向。"""
from support import Fixture,REPO,inventory
import json,os,pathlib,subprocess
f=Fixture('native-coexist');f.seed();f.cli('start');before=f.audit('before')
(f.home/'m3-isolated-validation').write_text('v0.25.2')
# 原生测试二进制由标准 cargo 构建，测试不会创建 WebView 或安装版 GUI。
build=subprocess.run(['cargo','test','-p','ez-assistant-desktop','--lib','--no-run','--offline','--message-format=json'],cwd=REPO,capture_output=True,text=True,timeout=180)
(f.root/'native-build.log').write_text(build.stderr);assert build.returncode==0,build.stderr
executable=None
for line in build.stdout.splitlines():
    row=json.loads(line)
    if row.get('reason')=='compiler-artifact' and row.get('executable') and row['target']['name']=='ez_assistant_desktop_lib':executable=row['executable']
assert executable
result=subprocess.run([executable,'runtime_bootstrap::tests::isolated_native_restart_preserves_the_running_host_source','--exact','--ignored','--nocapture'],env={**f.env,'EZ_ASSISTANT_TEST_RUNTIME_HOME':str(f.home)},capture_output=True,text=True,timeout=120)
(f.root/'native-test.log').write_text(result.stdout+result.stderr);assert result.returncode==0,result.stdout+result.stderr
assert '未启动' in f.cli('status');assert inventory(f.home/'data/runtime.sqlite3')==before
f.cli('start');f.cli('stop');assert f.audit('after')==before
print('PASS Client -> actual Desktop native restart -> Client; all 42 tables unchanged',flush=True)
