import subprocess,json,os
exe=os.environ['MINIFY_AUDIT_EXE']
cases={
'goto-forward':('function onTick()local x x=1 goto done x=2 ::done:: output.setNumber(1,x)end','final-dead-store-elimination'),
'goto-global-forward':('function onTick()x=1 goto done x=2 ::done:: output.setNumber(1,x)end','single-use-global-forwarding'),
'closure-table-param':('local function f(t)local g=function()return t.x end return t.x+g()end function onTick()output.setNumber(1,f({x=input.getNumber(1)}))end','table-parameter-scalarization'),
'carrier-shadow':('local tau=math.pi*2 function onTick()local tau=input.getNumber(1)output.setNumber(1,math.pi*2)output.setNumber(2,math.pi*2)output.setNumber(3,tau)end','immutable-global-expression-reuse'),
'root-global-collision':('x=0 function onDraw()x=x+1 output.setNumber(1,x)end local x=input.getNumber(1)function onTick()output.setNumber(2,x)end','root-local-globalization'),
'root-local-callback':('local function onTick()output.setNumber(1,5)end','root-local-globalization'),
'escaped-destructive-function':('v=100 function f(x)x=x+1 return x end local saved=f function onTick()v=f(1)v=f(2)output.setNumber(1,saved(10))end','destructive-result-globalization'),
'int64-table-key':('local t={[9007199254740992]=10,[9007199254740993]=20}function onTick()output.setNumber(1,t[9007199254740993])end','immutable-table-flattening'),
}
inputs=[]
for name,(s,p) in cases.items():
 for m in ['default','exact','safe']:
  inputs.append(dict(name=name,source=s,mode=m))
 inputs.append(dict(name=name,source=s,mode='default',only=[p]))
 inputs.append(dict(name=name,source=s,mode='default',toggles={p:False}))
p=subprocess.run([exe],input=''.join(json.dumps(v)+'\n'for v in inputs),capture_output=True,text=True,timeout=180)
f=open('docs/verification/minify-review-20261012/evidence/scope/findings-matrix.jsonl','w')
for inp,s in zip(inputs,p.stdout.splitlines()):
 v=json.loads(s);v['request']=inp;f.write(json.dumps(v,ensure_ascii=False)+'\n')
 print(inp['name'],inp['mode'],('only='+str(inp['only']) if 'only'in inp else 'toggles='+str(inp['toggles'])if 'toggles'in inp else 'all'), 'ERROR'if v.get('error')else 'DIFF'if v.get('before')!=v.get('after')else'OK',v.get('code'))
print('rc',p.returncode,'n',len(p.stdout.splitlines()),'stderr',p.stderr)
