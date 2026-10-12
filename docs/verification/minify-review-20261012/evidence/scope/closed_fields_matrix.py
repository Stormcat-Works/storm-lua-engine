import subprocess,json,collections,os
exe=os.environ['MINIFY_AUDIT_EXE']
cases={
'colon-method':'local t={longvalue=5}function t:verylongmethod(x)return self.longvalue+x end function onTick()output.setNumber(1,t:verylongmethod(3))end',
'alias-update':'local t={longvalue=5}local a=t function onTick()a.longvalue=a.longvalue+1 output.setNumber(1,t.longvalue)end',
'alias-identity':'local t={longvalue=5}local a=t function onTick()output.setBool(1,a==t)output.setNumber(1,a.longvalue)end',
'dynamic-literal':'local t={longvalue=5}local key="longvalue" function onTick()output.setNumber(1,t[key])end',
'dynamic-concat':'local t={longvalue=5}local key="long".."value" function onTick()output.setNumber(1,t[key])end',
'dynamic-format':'local t={longvalue=5}local key=string.format("%s%s","long","value")function onTick()output.setNumber(1,t[key])end',
'copy-pairs':'local t={longvalue=5,othervalue=6}local dest={}for k,v in pairs(t)do dest[k]=v end function onTick()output.setNumber(1,dest.longvalue+dest.othervalue)end',
'enumeration-key':'local t={longvalue=5}function onTick()for k,v in pairs(t)do output.setBool(1,k=="longvalue")output.setNumber(2,v)end end',
'string-dot':'local s="abc" local f=s.sub function onTick()output.setNumber(1,#f(s,1,2))end',
'string-colon':'function onTick()local s=property.getText("Title")output.setNumber(1,#s:sub(1,2))end',
'math-type-key':'local t={integer=5,float=6}function onTick()output.setNumber(1,t[math.type(1.0)])end',
'pack-count':'local t=table.pack(3,4,5)function onTick()output.setNumber(1,t.n)end',
'nested-alias':'local t={nested={longvalue=5}}local a=t.nested function onTick()a.longvalue=a.longvalue+1 output.setNumber(1,t.nested.longvalue)end',
'nested-identity':'local t={nested={longvalue=5}}function onTick()output.setBool(1,t.nested==t.nested)output.setNumber(2,t.nested.longvalue)end',
'closure-in-table':'local t={longfunction=function(x)return x+3 end}function onTick()output.setNumber(1,t.longfunction(4))end',
}
inputs=[dict(name=n,source=s,mode=m)for n,s in cases.items()for m in ['default','exact','safe','closed-fields']]
p=subprocess.run([exe],input=''.join(json.dumps(v)+'\n'for v in inputs),capture_output=True,text=True,timeout=180)
f=open('docs/verification/minify-review-20261012/evidence/scope/closed-fields-matrix.jsonl','w');summary=collections.Counter()
for inp,line in zip(inputs,p.stdout.splitlines()):
 v=json.loads(line);v['request']=inp;f.write(json.dumps(v)+'\n');status='error'if v.get('error')else'match'if v['before']==v['after']else'difference';summary[status]+=1
 if status!='match':print(json.dumps(v))
print('rc',p.returncode,'count',len(p.stdout.splitlines()),'summary',dict(summary),'stderr',p.stderr)
