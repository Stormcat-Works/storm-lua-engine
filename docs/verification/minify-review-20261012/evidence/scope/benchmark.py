import subprocess,json,statistics,os
inputs=[]
for n in [50,100,200,400,800,1600]:
 src='local t={x=123} function onTick()'+('output.setNumber(1,t.x) '*n)+'end'
 for mode in ['identity','immutable-table']:
  for repeat in range(5): inputs.append(dict(n=n,source=src,mode=mode,repeat=repeat,bench=True))
p=subprocess.run([os.environ['MINIFY_AUDIT_EXE']],input=''.join(json.dumps(v)+'\n'for v in inputs),capture_output=True,text=True,timeout=180)
values={};f=open('docs/verification/minify-review-20261012/evidence/scope/table-flatten-timing.jsonl','w')
for inp,line in zip(inputs,p.stdout.splitlines()):
 v=json.loads(line);v.update(n=inp['n'],repeat=inp['repeat'],input_bytes=len(inp['source']));f.write(json.dumps(v)+'\n');values.setdefault((inp['n'],inp['mode']),[]).append(v['transform_us'])
print('rc',p.returncode,'samples',len(p.stdout.splitlines()),'stderr',p.stderr)
for (n,m),vs in values.items():print(n,m,'median_us',statistics.median(vs),'range_us',[min(vs),max(vs)])
