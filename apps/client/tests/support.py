"""正式 Client 的隔离验收支持；所有数据与独立备份保留，不访问用户 Runtime Home。"""
import errno, fcntl, hashlib, json, os, pathlib, pty, re, select, shutil, signal, socket, sqlite3, struct, subprocess, tempfile, termios, time, urllib.request
ROOT = pathlib.Path(__file__).resolve().parents[1]
REPO = ROOT.parents[1]
NODE = shutil.which('node')
HOST = pathlib.Path(os.environ.get('EZ_ASSISTANT_RUNTIME_EXECUTABLE', REPO/'target/debug/ez-assistant-runtime')).resolve()
OPENER = urllib.request.build_opener(urllib.request.ProxyHandler({}))
ANSI = re.compile(r'\x1b\[[0-?]*[ -/]*[@-~]')
class Fixture:
    def __init__(self, name):
        self.root = pathlib.Path(tempfile.mkdtemp(prefix='ez-client-'+name+'-')).resolve()
        self.home = self.root/'home'; self.user = self.root/'user'; self.user.mkdir(mode=0o700)
        self.source = self.root/'ez-assistant-runtime'; shutil.copy2(HOST,self.source); self.source.chmod(0o700)
        self.env = {**os.environ,'HOME':str(self.user),'TMPDIR':str(self.root),'EZ_ASSISTANT_RUNTIME_HOME':str(self.home),'EZ_ASSISTANT_RUNTIME_EXECUTABLE':str(self.source),'TERM':'xterm-256color','SSH_CONNECTION':'isolated-no-browser'}
        self.logs=[]
        print('FIXTURE '+str(self.root),flush=True)
    def cli(self,*args,expected=0):
        result=subprocess.run([NODE,str(ROOT/'dist/cli.js'),*args],env={**self.env,'NO_COLOR':'1'},capture_output=True,text=True,timeout=100)
        self.logs.append({'args':args,'code':result.returncode,'output':result.stdout+result.stderr})
        (self.root/'commands.json').write_text(json.dumps(self.logs,ensure_ascii=False,indent=2))
        assert result.returncode==expected,result.stdout+result.stderr
        return result.stdout+result.stderr
    def helper(self,operation,body=None):
        result=subprocess.run([str(self.source),'access',operation,'--runtime-home',str(self.home)],env=self.env,input=json.dumps(body) if body else '',capture_output=True,text=True,timeout=15)
        assert result.returncode==0,result.stderr
        return json.loads(result.stdout)
    def seed(self,password=False):
        with socket.socket() as sock:sock.bind(('127.0.0.1',0));port=sock.getsockname()[1]
        configuration={'port':port,'scheme':'http','remote_enabled':False,'server_names':[],'tls_certificate':None,'tls_private_key':None}
        current=self.helper('configure',{'expected_revision':None,'configuration':configuration})
        if password:self.helper('set-password',{'expected_revision':current['revision'],'password':'isolated-client-password'})
        assert not (self.home/'data').exists()
    def discovery(self):return json.loads((self.home/'run/runtime.json').read_text())
    def request(self,path,body=None):
        d=self.discovery()
        req=urllib.request.Request(d['address']+path,headers={'Authorization':'Bearer '+d['access_token'],'X-Ez-Client-Version':'0.25.2','X-Ez-Min-Compatible-Version':'0.25.2','Content-Type':'application/json'},data=json.dumps(body).encode() if body is not None else None)
        with OPENER.open(req,timeout=5) as res:return json.load(res)
    def runtime(self,kind,payload=None):return self.request('/commands',{'request_id':'m4-'+kind,'command':{'scope':'runtime','payload':{'type':kind,'payload':payload or {}}}})['result']['payload']['payload']
    def access(self,kind,payload=None):return self.request('/commands',{'request_id':'m4-'+kind,'command':{'scope':'host_access','payload':{'type':kind,**({'payload':payload} if payload is not None else {})}}})['result']['payload']
    def audit(self,name):
        db=self.home/'data/runtime.sqlite3'; destination=self.root/(name+'.sqlite3')
        assert not destination.exists()
        before=inventory(db)
        source=sqlite3.connect(db.as_uri()+'?mode=ro',uri=True);target=sqlite3.connect(destination)
        try:source.backup(target)
        finally:target.close();source.close()
        assert inventory(destination)==before
        (self.root/(name+'.json')).write_text(json.dumps({'database':str(db),'backup':str(destination),'inventory':before},ensure_ascii=False,indent=2))
        return before

def inventory(db):
    assert pathlib.Path(tempfile.gettempdir()).resolve() in pathlib.Path(db).resolve().parents
    c=sqlite3.connect(pathlib.Path(db).resolve().as_uri()+'?mode=ro',uri=True)
    try:
        c.execute('BEGIN');assert c.execute('PRAGMA integrity_check').fetchall()==[('ok',)]
        result={}
        for (name,) in c.execute("SELECT name FROM sqlite_schema WHERE type='table' ORDER BY name").fetchall():
            table='"'+name.replace('"','""')+'"'
            count=c.execute('SELECT COUNT(*) FROM '+table).fetchone()[0]
            rows=sorted(repr(r) for r in c.execute('SELECT * FROM '+table))
            result[name]={'count':count,'fields_sha256':hashlib.sha256('\n'.join(rows).encode()).hexdigest()}
        assert len(result)==42
        return result
    finally:c.close()

class Terminal:
    def __init__(self,fixture,args=None,columns=80):
        self.fixture=fixture
        self.master,slave=pty.openpty();fcntl.ioctl(slave,termios.TIOCSWINSZ,struct.pack('HHHH',32,columns,0,0))
        def setup():os.setsid();fcntl.ioctl(slave,termios.TIOCSCTTY,0)
        self.process=subprocess.Popen([NODE,str(ROOT/'dist/cli.js'),*(args or ['config'])],stdin=slave,stdout=slave,stderr=slave,env=fixture.env,preexec_fn=setup)
        os.close(slave);self.data=b'';self.cursor=0
    @property
    def output(self):return ANSI.sub('',self.data.decode('utf8',errors='replace'))
    def read(self,timeout=.1):
        if select.select([self.master],[],[],timeout)[0]:
            try:self.data+=os.read(self.master,65536)
            except OSError as error:
                if error.errno!=errno.EIO:raise
    def expect(self,text,timeout=15):
        deadline=time.monotonic()+timeout
        while time.monotonic()<deadline:
            index=self.output.find(text,self.cursor)
            if index>=0:self.cursor=index+len(text);time.sleep(.1);return
            self.read()
        raise AssertionError(f'missing {text!r}\n{self.output[-2500:]}')
    def send(self,text):os.write(self.master,text.encode());time.sleep(.08)
    def choose(self,index):
        for _ in range(index):self.send('\x1b[B')
        self.send('\r')
    def replace(self,text):self.send('\x01\x0b'+text+'\r')
    def enter_access(self):self.expect('选择配置范围');self.choose(0);self.expect('选择要配置的项目')
    def exit_access(self):self.choose(8);self.expect('选择配置范围');self.choose(2);self.finish()
    def finish(self,code=0):
        deadline=time.monotonic()+10
        while self.process.poll() is None and time.monotonic()<deadline:self.read()
        self.read(0)
        assert self.process.poll()==code,(self.process.poll(),self.output[-2500:])
    def close(self,name):
        if self.process.poll() is None:
            os.kill(self.process.pid,signal.SIGINT)
            try:self.process.wait(timeout=16)
            except subprocess.TimeoutExpired:raise AssertionError('Client did not finish; preserve helper and fixture')
        os.close(self.master)
        (self.fixture.root/(name+'.txt')).write_text(self.output)
