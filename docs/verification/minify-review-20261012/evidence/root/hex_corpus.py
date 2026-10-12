#!/usr/bin/env python3
import json,subprocess,random,sys
from pathlib import Path
rng=random.Random(20261012)
vals=['0x.8p1024','0x2p-1075','0x1.0000000000000800001p0','0x1.fffffffffffff8p0','0x100000000000000001','0xfffffffffffffffff']
for _ in range(3000):
 mant=''.join(rng.choice('0123456789abcdef') for _ in range(rng.randrange(1,30)))
 split=rng.randrange(len(mant)+1);mant=mant[:split]+'.'+mant[split:]
 exponent=rng.choice([-1078,-1075,-1074,-1024,-64,-4,0,4,64,1000,1024])
 vals.append('0x'+mant+'p'+str(exponent))
requests=[dict(action='print',source='return '+v) for v in vals]
r=subprocess.run([sys.argv[1]],input='\n'.join(map(json.dumps,requests))+'\n',text=True,capture_output=True,check=True,timeout=120)
fails=[]
for q,l in zip(requests,r.stdout.split('\n')[:-1]):
 o=json.loads(l)
 if o.get('original',{}).get('ok') and o.get('original')!=o.get('generated'):fails.append(dict(request=q,response=o))
out=Path(sys.argv[2]);out.mkdir(parents=True,exist_ok=True)
out.joinpath('hex-corpus-summary.json').write_text(json.dumps({'seed':20261012,'requests':len(requests),'mismatches':len(fails),'rc':r.returncode},indent=2)+'\n')
out.joinpath('hex-corpus-failures.jsonl').write_text('\n'.join(map(json.dumps,fails))+'\n')
