#!/usr/bin/env python3
import json,subprocess,random,sys
from pathlib import Path
rng=random.Random(15102026)
atoms=['0','1','2','-1','1.0','0.5','true','false','nil','"x"','"a\\n"','9007199254740993']
ops=['+','-','*','/','//','%','^','..','<<','>>','&','|','~','==','~=','<','>=','and','or']
def expr(d):
 if d==0 or rng.random()<.4:return rng.choice(atoms)
 if rng.random()<.25:return rng.choice(['not ','-','~','#'])+'('+expr(d-1)+')'
 return '('+expr(d-1)+' '+rng.choice(ops)+' '+expr(d-1)+')'
requests=[]
for i in range(500):
 source='return '+expr(3)
 for options in [{},{'sourceMap':True},{'numericMode':'exact'},{'numericMode':'exact','sourceMap':True},{'targetSize':0},{'targetSize':0,'sourceMap':True}]:requests.append({'case':i,'source':source,'options':options})
r=subprocess.run([sys.argv[1]],input='\n'.join(map(json.dumps,requests))+'\n',text=True,capture_output=True,check=True,timeout=180)
rows=[dict(request=q,response=json.loads(l)) for q,l in zip(requests,r.stdout.split('\n')[:-1])];fails=[]
for i in range(0,len(rows),6):
 a=rows[i:i+6]
 for j in [0,2,4]:
  x=a[j]['response'].get('result',{});y=a[j+1]['response'].get('result',{})
  if x.get('code')!=y.get('code') or x.get('ok')!=y.get('ok'):fails.append([a[j],a[j+1]])
summary={'seed':15102026,'cases':500,'compileCalls':len(rows),'sourceMapPairs':1500,'onOffDifferences':len(fails),'panics':sum(x['response'].get('panic',False)for x in rows),'compileFailures':sum(x['response'].get('result',{}).get('ok')is False for x in rows),'returncode':r.returncode}
out=Path(sys.argv[2]);out.mkdir(parents=True,exist_ok=True);out.joinpath('map-parity-summary.json').write_text(json.dumps(summary,indent=2)+'\n');out.joinpath('map-parity-differences.json').write_text(json.dumps(fails,indent=2)+'\n')
