import json,subprocess,random,time,sys
from pathlib import Path
BIN=sys.argv[1]
OUT=Path(sys.argv[2]); OUT.mkdir(parents=True,exist_ok=True)
rng=random.Random(20261012)
cases=[]
for i in range(256):
 for form in [f'"\\{i:03d}"',f'"\\x{i:02x}"']:
  cases.append(('byte-escape',f'return {form}'))
for cp in [0,1,32,65,127,128,255,256,0x7ff,0x800,0xffff,0x10000,0x1f600,0x10ffff]:
 cases.append(('unicode-escape',f'return "\\u{{{cp:X}}}"'))
for prefix in ['', '\n', '\r','\r\n','\n\r']:
 for newline in ['\n','\r','\r\n','\n\r']:
  for level in range(3):
   cases.append(('long-string-newline','return ['+'='*level+'['+prefix+'a'+newline+'b'+']'+'='*level+']'))
for whitespace in [' ','\t','\n','\r','\r\n','\n\r','\u00a0','\u3000']:
 cases.append(('z-whitespace','return "a\\z'+whitespace+'b"'))
for h in ['0x1.','0xA.','0x1.p4','0x1.p-4','0x.1p4','0x10000000000000000','0x1ffffffffffffffff','0x100000000000000001','0xffffffffffffffffffff','0x1p1024','0x.8p1024','0x2p-1075']:
 cases.append(('hex-literal','return '+h))
for ending in ['\n','\r','\r\n','\n\r']:
 cases.append(('line-comment','-- audit'+ending+'return 42'))
ops=['+','-','*','/','//','%','^','..','<<','>>','&','|','~','==','~=','<','>','<=','>=','and','or']
atoms=['0','1','2','3','-1','1.0','0.5','2.5','true','false','nil','"x"','""','0x1.','0x1p1','1e999']
def expr(d):
 if d==0 or rng.random()<.4:return rng.choice(atoms)
 if rng.random()<.2:return rng.choice(['not ','-','~','#'])+'('+expr(d-1)+')'
 s=expr(d-1)+' '+rng.choice(ops)+' '+expr(d-1)
 return '('+s+')' if rng.random()<.6 else s
for i in range(12000):cases.append(('generated-expression','return '+expr(3)))
req=[]
for family,source in cases:req.append({'family':family,'action':'print','source':source})
# Public API controls on each lexical family, and strict safe/exact comparisons.
for family,source in cases:
 if family!='generated-expression':
  for options in [{'mode':'safe','numericMode':'exact'},{'mode':'safe','numericMode':'exact','sourceMap':True},{'environment':'extended'}]:
   req.append({'family':family,'source':source,'options':options})
start=time.monotonic();r=subprocess.run([BIN],input='\n'.join(json.dumps(x) for x in req)+'\n',text=True,capture_output=True,timeout=180)
fails=[];counts={};valid=0
for q,l in zip(req,r.stdout.split('\n')[:-1]):
 o=json.loads(l);a=o.get('original');b=o.get('generated')
 if not (a or {}).get('ok'):continue
 valid+=1
 if a!=b:
  fam=q['family'];counts[fam]=counts.get(fam,0)+1
  c=o.get('result',{});fails.append({'request':q,'original':a,'generated':b,'compileOk':c.get('ok',o.get('ok')),'code':c.get('code',o.get('code')),'diagnostics':c.get('diagnostics'),'mapProduced':'map'in c})
summary={'seed':20261012,'requests':len(req),'responses':len(r.stdout.split('\n')[:-1]),'validOriginalExecutions':valid,'mismatches':len(fails),'mismatchesByFamily':counts,'returncode':r.returncode,'seconds':time.monotonic()-start,'stderr':r.stderr[:1000]}
OUT.joinpath('syntax-corpus-summary.json').write_text(json.dumps(summary,indent=2)+'\n')
OUT.joinpath('syntax-corpus-failures.jsonl').write_text('\n'.join(map(json.dumps,fails))+'\n')
print(json.dumps(summary,indent=2))
