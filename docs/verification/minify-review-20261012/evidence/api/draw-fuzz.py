import random,json,re
from pathlib import Path
rng=random.Random(20261012)
import sys
root=Path(sys.argv[1]).resolve()
ids=re.findall(r'^   [0-9]+ => "([a-z-]+)",',(root/'crates/storm-lua-minify/src/pass_ids.rs').read_text(),re.M)
setup='trace={} screen={} for _,name in ipairs({"setColor","drawLine","drawRectF","drawCircleF","drawText"})do screen[name]=function(...)local a={name} for i=1,select("#",...)do local v=select(i,...) a[#a+1]=type(v)..":"..tostring(v)end trace[#trace+1]=table.concat(a,"|")end end'
cs=[]
for seed in range(120):
 arities={'drawLine':4,'drawRectF':4,'drawCircleF':3,'drawText':3}
 kind=rng.choice(list(arities));n=rng.choice([4,8,16,24,32,48,64,90]);variant=rng.choice(['linear','periodic','random','repeat','translations'])
 calls=[]
 for i in range(n):
  args=[]
  for a in range(arities[kind]):
   x=i*(a+1)+7 if variant=='linear' else (i*3+(i//4)*10+a) if variant=='periodic' else rng.randrange(-100,101) if variant=='random' else (i%4)*30+a*8 if variant=='repeat' else (i//6)*100+(i%6)*3+a
   args.append(str(x))
  if kind=='drawText':args[-1]=rng.choice(['"abc"','"\\000\\255\\127"','"雪😀"','"repeated-literal-text"'])
  calls.append('screen.'+kind+'('+','.join(args)+')')
 s='function onDraw()'+''.join(calls)+'end'
 for p in ['records','screen','screen-periodic','motifs','compile']:
  cs.append(dict(name=f'fuzz-{seed}-{variant}-{kind}-{n}-{p}',source=s,setup=setup,after='onDraw();return table.concat(trace,";")',**{'pass':p}))
# High integer text allowed by the drawText API, preserve exact tostring.
for values in [[9007199254740992,9007199254740993]*4,list(range(9007199254740990,9007199254741006)),[9223372036854775807]*6]:
 s='function onDraw()'+''.join(f'screen.drawText(0,{i*5},{x})' for i,x in enumerate(values))+'end'
 for p in ['records','screen','compile']:
  for opts in [{},{'mode':'safe','numericMode':'exact'},{'numericMode':'exact','passToggles':{k:k in ['ordered-screen-loop-synthesis','scope-renaming'] for k in ids}}]:cs.append(dict(name=f'integer-{values[0]}-{len(values)}-{p}-{len(opts)}',source=s,setup=setup,after='onDraw();return table.concat(trace,";")',options=opts,**{'pass':p}))
for c in cs:print(json.dumps(c))
