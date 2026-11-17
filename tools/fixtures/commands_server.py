import json,sys,time,pathlib,subprocess,os
mode=sys.argv[1];opened={}
calls=pathlib.Path('calls')
if '--calls-dir' in sys.argv:
    calls=pathlib.Path(sys.argv[sys.argv.index('--calls-dir')+1])/pathlib.Path.cwd().name
def send(value):
    body=json.dumps({'jsonrpc':'2.0',**value}).encode()
    sys.stdout.buffer.write(('Content-Length: %d\r\n\r\n'%len(body)).encode()+body);sys.stdout.buffer.flush()
while True:
    headers={}
    while line:=sys.stdin.buffer.readline():
        if line==b'\r\n':break
        name,value=line.decode().split(':',1);headers[name.lower()]=value.strip()
    if not line:break
    message=json.loads(sys.stdin.buffer.read(int(headers['content-length'])))
    method=message.get('method');params=message.get('params',{});result=None
    with calls.open('a') as log:log.write(method+'\n')
    if method=='initialize':result={'capabilities':{'textDocumentSync':1,'codeActionProvider':True,'executeCommandProvider':{'commands':['fixture.apply']}}}
    elif method in ('textDocument/didOpen','textDocument/didChange'):
        document=params['textDocument'];opened[document['uri']]=document.get('text',params.get('contentChanges',[{}])[0].get('text'))
    elif method=='textDocument/didClose':opened.pop(params['textDocument']['uri'],None)
    elif method=='textDocument/codeAction':
        uri=params['textDocument']['uri'];edit={'changes':{uri:[{'range':{'start':{'line':0,'character':0},'end':{'line':0,'character':4}},'newText':'next'}]}}
        result=[{'title':'Edit then run','edit':edit,'command':{'title':'','command':'fixture.apply','arguments':[{'marker':17}]}},
                {'title':'Unsupported whole action','edit':edit,'command':{'command':'not-advertised'}}]
        if mode=='edit-only':result[0].pop('command')
        if mode=='command-only':result[0].pop('edit')
        if mode in ('create','create-tree'):
            folder=uri.rsplit('/',1)[0]
            destination='/fresh/created.json' if mode=='create-tree' else '/created.json'
            result[0]['edit']={'documentChanges':[{'kind':'create','uri':folder+destination},
                {'textDocument':{'uri':uri,'version':None},'edits':edit['changes'][uri]}]}
        if mode in ('inputs','inputs-edit-only','inputs-overwrite'):
            folder=uri.rsplit('/',1)[0]
            result[0]['edit']={'documentChanges':[{'kind':'rename','oldUri':folder+'/settings-old.json','newUri':folder+'/settings-new.json'},
                {'textDocument':{'uri':uri,'version':None},'edits':edit['changes'][uri]}]}
        if mode=='inputs-overwrite':result[0]['edit']['documentChanges'][0]['options']={'overwrite':True}
        if mode=='inputs-edit-only':result[0].pop('command')
        if mode=='affected-text':
            folder=uri.rsplit('/',1)[0]
            result[0]['edit']={'changes':{folder+'/settings-old.json':[{'range':{'start':{'line':0,'character':0},'end':{'line':0,'character':2}},'newText':'[]'}]}}
            result[0].pop('command')
        if mode=='move-workspace':
            folder=uri.rsplit('/',1)[0]
            result.append({'title':'Move server workspace','edit':{'documentChanges':[{'kind':'rename','oldUri':folder,'newUri':folder+'-moved'}]},'command':{'command':'fixture.apply'}})
    elif method=='workspace/executeCommand':
        assert params=={'command':'fixture.apply','arguments':[{'marker':17}]}
        assert list(opened.values())==[('text' if mode=='command-only' else 'next')+' 文😀\r\n']
        if mode=='inputs':assert pathlib.Path('settings-new.json').exists() and not pathlib.Path('settings-old.json').exists()
        with open('executions','a') as log:log.write('executed\n')
        if mode in ('fork-block','retained-result','retained-error'):
            subprocess.Popen([sys.executable,'-c',"import signal,pathlib,time,os\nsignal.signal(signal.SIGTERM,signal.SIG_IGN)\npathlib.Path('child-ready').write_text(str(os.getpid()))\nvalue=0\nwhile True:\n pathlib.Path('heartbeat').write_text(str(value));value+=1;time.sleep(.01)\n"])
            if mode=='fork-block':time.sleep(60)
            else:
                pathlib.Path('owner.tmp').write_text(json.dumps({'watcher':os.getppid(),'server':os.getpid()}))
                pathlib.Path('owner.tmp').replace('owner.json')
                deadline=time.monotonic()+20
                while not pathlib.Path('reply-now').exists() and time.monotonic()<deadline:time.sleep(.01)
                if not pathlib.Path('reply-now').exists():raise RuntimeError('Retained-result supervisor did not request a reply')
                if mode=='retained-error':
                    send({'id':message['id'],'error':{'code':-32801,'message':'Retained command error 文😀 "quoted" \\ original'}});continue
        if mode=='block':time.sleep(60)
        if mode=='cancel':send({'id':message['id'],'error':{'code':-32801,'message':'Changed index after command effects'}});continue
        time.sleep(.1);result={'marker':17,'source':list(opened.values())[0]}
        if mode=='retained-result':result['kept']='原本 😀 "quoted" \\ exact\r\n'*1024
    if 'id' in message:send({'id':message['id'],'result':result})
