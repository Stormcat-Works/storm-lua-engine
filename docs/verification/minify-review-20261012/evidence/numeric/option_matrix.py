import itertools,json,subprocess,collections,time,sys
from pathlib import Path
sources=[
('negative-divisor','return 5//-2,5%-2'),
('shift','return 1<<64,4>>-1,~4294967296.0'),
('int64-panic','return 0x8000000000000000//-1'),
('integer-compare','return 9007199254740993>9007199254740992,9007199254740993==9007199254740992.0'),
('coercion','return true+1'),
('subtype-safe','return math.type(1/1)'),
('coercion-identity','local x="3" return x*1'),
('quotient-overflow','local x=0x8000000000000000 local r=x%10 x=(x-r)/10 return x,r'),
('signed-stale','local x=1 local a,b,c x=x*12345 a=x*12345 b=x*12345 c=x*12345 return x,a,b,c'),
('split-effects','local n=0 local function f() n=n+1 return n end local x=2 local r=math.max(x,0)^2*f()-math.min(x,0)^2*f() return r,n'),
('split-capture','local p=99 local function f(x) p=math.max(x,0) x=math.min(x,0) x=p+x return x end local x=f(5) return x,p'),
('split-overflow','local x=4000000000 local r=math.max(x,0)^2-math.min(x,0)^2 return r'),
('induction-subtype','for i=1.0,1.0 do return math.type(math.floor(i)),math.type(i) end'),
('boolean-subtype','local x=3 local bit=x&1>0 return math.type(bit and 1.0 or 0.0)'),
('format-panic','return string.format("%018446744073709551615d",1)'),
]
reqs=[]
for (case,source),mode,fast,target,origin in itertools.product(sources,['default','safe','exact','safe_exact'],[False,True],[None,0,10000],[False,True]):
 d=dict(case=case,source=source,mode=mode,fast=fast,origin=origin)
 if target is not None:d['target']=target
 reqs.append(d)
start=time.monotonic()
p=subprocess.run([sys.argv[1]],input=''.join(json.dumps(x)+'\n' for x in reqs),text=True,capture_output=True,check=True)
rows=[json.loads(x) for x in p.stdout.splitlines()];assert len(rows)==len(reqs)
counts=collections.Counter()
for req,row in zip(reqs,rows):
 row['case']=req['case']
 row['request']=req
 if row.get('same'):kind='same'
 elif 'panic' in row:kind='panic'
 elif 'compile_error' in row:kind='compile_error'
 elif row['expected'].startswith('ERROR:') and row['actual'].startswith('ERROR:'):kind='both_runtime_error'
 else:kind='different'
 counts[(row['case'],row['mode'],req.get('target'),kind)]+=1
 row.pop('passes',None)
Path('option-results.jsonl').write_text(''.join(json.dumps(x)+'\n' for x in rows))
summary=dict(cases=len(reqs),elapsed_seconds=time.monotonic()-start,counts={f'{case}/{mode}/target={target}/{kind}':n for (case,mode,target,kind),n in counts.items()},notes=['origin=true means CompileOptions.origin_source provenance tracking, not the build SDK Source Map API.','fast and origin each tested false/true; each summary count of4 represents these combinations.'])
Path('option-summary.json').write_text(json.dumps(summary,indent=2))
print(json.dumps({k:v for k,v in summary.items() if k!='counts'},indent=2))
