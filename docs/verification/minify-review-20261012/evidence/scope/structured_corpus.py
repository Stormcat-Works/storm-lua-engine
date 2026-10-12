import subprocess,json,collections,os
exe=os.environ['MINIFY_AUDIT_EXE']
templates={
'local-shadow':'local a={v} local function outer()return a end local a={w} function onTick()output.setNumber(1,outer())output.setNumber(2,a)end',
'root-global-shadow':'a={v} function onDraw()a=a+1 output.setNumber(1,a)end local a=input.getNumber(1)+{w} function onTick()output.setNumber(2,a)end',
'closure-loop':'local out={{}} for i=1,3 do local a=i+{v} out[i]=function()return a end end function onTick()for i=1,3 do output.setNumber(i,out[i]())end end',
'closure-mutated':'local function f(a)local b=a+{v}local get=function()return b end b=b+{w} return get end local g=f(1)function onTick()output.setNumber(1,g())end',
'closure-snapshot':'local a={v} local function f()local b=a return function()return b end end local g=f() a={w} function onTick()output.setNumber(1,g())end',
'closure-single-return-table':'local a={v} local function f()local b=a return {{get=function()return b end}} end local g=f() a={w} function onTick()output.setNumber(1,g.get())end',
'param-capture':'local function f(t)local g=function()return t.x end return t.x+g()end function onTick()output.setNumber(1,f({{x={v}}}))end',
'param-mutate':'local function f(t)t.x=t.x+{v} t.y=t.x+{w} return t.x+t.y end function onTick()output.setNumber(1,f({{x=2,y=3}}))end',
'param-reverse':'local function f(t)t.x,t.y=t.y,t.x return t.x*10+t.y end function onTick()output.setNumber(1,f({{x={v},y={w}}}))end',
'param-fresh':'local function f(t)return t.x~=t.y end function onTick()output.setBool(1,f({{x={{a={v}}},y={{a={v}}}}}))end',
'table-fresh':'local function f()return {{x={v}}}end function onTick()local a=f()local b=f()a.x={w} output.setBool(1,a~=b)output.setNumber(2,b.x)end',
'root-table-mutate':'local t={{x={v},y={w}}} function onTick()t.x=t.x+1 output.setNumber(1,t.x)end function onDraw()output.setNumber(2,t.y)end',
'namespace-mutual':'local ns={{}} function ns.f(x)if x==0 then return {v} end return ns.g(x-1)end function ns.g(x)if x==0 then return {w} end return ns.f(x-1)end function onTick()output.setNumber(1,ns.f(3))end',
'multi-return-local':'local function f()return {v},{w},99 end function onTick()local a,b=f()output.setNumber(1,a)output.setNumber(2,b)end',
'multi-return-packed':'local function f()return {v},{w},99 end function onTick()local a=f()local b={w}output.setNumber(1,a)output.setNumber(2,b)end',
'multi-return-sink':'local function f()return {v},{w},99 end function onTick()local a=f()output.setNumber(1,a)end',
'parallel-read-before-write':'local a={v} function onTick()local b=a a,b={w},a output.setNumber(1,a)output.setNumber(2,b)end',
'forward-goto':'function onTick()local a a={v} goto done a={w} ::done:: output.setNumber(1,a)end',
'backward-goto':'function onTick()local a={v} local n=0 ::again:: n=n+1 output.setNumber(1,a)local b={w}output.setNumber(2,b)if n<2 then goto again end end',
'repeat-condition':'function onTick()local n=0 repeat local step={v}+1 n=n+step until n>step*2 output.setNumber(1,n)end',
'loop-local-reuse':'function onTick()local a={v}local n=0 while n<3 do output.setNumber(1,a)local b={w}output.setNumber(2,b)n=n+1 end end',
'for-closure-capture':'function onTick()local t={{}}for i=1,3 do t[i]=function()return i+{v} end end for i=1,3 do output.setNumber(i,t[i]())end end',
'root-function-alias':'v={v} function f(x)x=x+{w} return x end local a=f function onTick()v=f(1)v=f(2)output.setNumber(1,a(10))end',
'local-carrier-shadow':'local tau=math.pi*{v} function onTick()local tau={w}+input.getNumber(1)output.setNumber(1,math.pi*{v})output.setNumber(2,math.pi*{v})output.setNumber(3,tau)end',
}
inputs=[]
for name,t in templates.items():
 for v,w in [(0,1),(1,2),(3,7),(7,2),(19,999)]:
  s=t.format(v=v,w=w)
  for m in ['default','exact','safe','rename']:
   inputs.append(dict(name=name,values=[v,w],source=s,mode=m))
p=subprocess.run([exe],input=''.join(json.dumps(v)+'\n'for v in inputs),capture_output=True,text=True,timeout=180)
f=open('docs/verification/minify-review-20261012/evidence/scope/structured-corpus.jsonl','w')
counts=collections.Counter();fail=collections.Counter();errs=collections.Counter()
for inp,s in zip(inputs,p.stdout.splitlines()):
 v=json.loads(s);v['request']=inp;f.write(json.dumps(v,ensure_ascii=False)+'\n');counts[inp['mode']]+=1
 if v.get('error'):errs[(inp['name'],inp['mode'])]+=1
 elif v.get('before')!=v.get('after'):fail[(inp['name'],inp['mode'])]+=1
print('rc',p.returncode,'n',len(p.stdout.splitlines()),'expected',len(inputs),'stderr',p.stderr)
print('counts',dict(counts));print('errors',dict(errs));print('differences',dict(fail))
summary=dict(total=len(inputs),actual=len(p.stdout.splitlines()),rc=p.returncode,counts=dict(counts),errors=[list(k)+[v]for k,v in errs.items()],differences=[list(k)+[v]for k,v in fail.items()])
open('docs/verification/minify-review-20261012/evidence/scope/corpus-summary.json','w').write(json.dumps(summary,ensure_ascii=False,indent=2)+'\n')
