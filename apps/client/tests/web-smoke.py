from support import Fixture,ROOT,NODE
from http.server import BaseHTTPRequestHandler,ThreadingHTTPServer
import json,os,pathlib,subprocess,threading
class Provider(BaseHTTPRequestHandler):
    def log_message(self,*args):pass
    def do_POST(self):
        self.rfile.read(int(self.headers.get('content-length',0)))
        assert self.path=='/v1/chat/completions'
        self.send_response(200);self.send_header('Content-Type','text/event-stream');self.end_headers()
        for choice in [{'index':0,'delta':{'role':'assistant','content':'Client 启动的 Host 已完成 Agent 回复。'},'finish_reason':None},{'index':0,'delta':{},'finish_reason':'stop'}]:
            self.wfile.write(('data: '+json.dumps({'id':'m4-offline-response','model':'offline-model','choices':[choice],'usage':{'prompt_tokens':30,'completion_tokens':15,'total_tokens':45}})+'\n\n').encode());self.wfile.flush()
        self.wfile.write(b'data: [DONE]\n\n')
    def do_GET(self):
        self.send_response(200);self.send_header('Content-Type','application/json');self.end_headers();self.wfile.write(b'{"object":"list","data":[{"id":"offline-model"}]}')
f=Fixture('web');f.seed(password=True);f.cli('start');f.audit('before')
provider=ThreadingHTTPServer(('127.0.0.1',0),Provider);threading.Thread(target=provider.serve_forever,daemon=True).start()
print('Non-production fixture writes: provider=1, fixed model=1, workspace=1, user session=1, Agent input/run=1; local mock only',flush=True)
try:
    p=f.runtime('create_provider',{'connection':{'display_name':'M4 本地测试','provider_type':'local','endpoint':f'http://127.0.0.1:{provider.server_port}/v1','protocol_preference':'chat_completions','models_path':'/v1/models','discovery_format':'openai'},'credential':{'mode':'replace','value':'m4-not-a-real-secret'}})
    selection={'provider_instance_id':p['provider_instance_id'],'model_id':'offline-model'}
    parameters={'context_window_tokens':{'state':'known','value':16384},'max_output_tokens':{'state':'known','value':4096},'max_input_tokens':{'state':'unknown'},'reasoning_max_input_tokens':{'state':'unknown'},'reasoning_max_output_tokens':{'state':'unknown'},'streaming':'supported','image_input':'unsupported','tool_calls':'supported','reasoning':'unsupported','tool_choice':{k:'supported' for k in ['auto','none','required','named']},'tool_image_projection':'unsupported','reasoning_mode':'unsupported','reasoning_efforts':{},'default_reasoning_effort':None}
    f.runtime('save_model_fixed_config',{'origin':'manual','selection':selection,'parameters':parameters});f.runtime('set_default_model',{'selection':selection})
    workspace=f.root/'workspace';workspace.mkdir()
    w=f.runtime('register_workspace',{'label':'M4 临时工作空间','primary_directory':str(workspace),'additional_directories':[]})
    f.runtime('create_session',{'title':'Client M4 临时会话','model_selection':None,'workspace_id':w['workspace']['workspace_id']})
    f.cli('web')
    env={**f.env,'EZ_ASSISTANT_TEST_RUNTIME_HOME':str(f.home),'EZ_ASSISTANT_TEST_SCREENSHOT':str(f.root/'web.png'),'PLAYWRIGHT_BROWSERS_PATH':os.environ.get('PLAYWRIGHT_BROWSERS_PATH',str(pathlib.Path.home()/'Library/Caches/ms-playwright'))}
    result=subprocess.run([NODE,str(ROOT/'tests/web-smoke.mjs')],env=env,capture_output=True,text=True,timeout=90)
    (f.root/'web-test.log').write_text(result.stdout+result.stderr);assert result.returncode==0,result.stdout+result.stderr
    f.cli('stop');after=f.audit('after')
    for table,count in [('sessions',2),('runs',1),('inputs',1),('providers',1),('model_fixed_configs',1),('workspaces',1)]:assert after[table]['count']==count,(table,after[table])
    print('PASS Web login / Agent / logout and final 42-table independent backup',flush=True)
finally:provider.shutdown();provider.server_close()
