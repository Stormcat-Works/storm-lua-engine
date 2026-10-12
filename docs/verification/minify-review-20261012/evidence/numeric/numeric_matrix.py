import itertools,json,subprocess,time,collections,sys
from pathlib import Path
vals=['0','1','-1','2','-2','5','-5','63','64','-64','65','4294967296','9007199254740992','9007199254740993','0x7fffffffffffffff','0x8000000000000000','0xffffffffffffffff','0.0','-0.0','1.0','1.5','5.5']
ops=['+','-','*','/','//','%','^','==','~=','<','<=','>','>=','&','|','~','<<','>>']
requests=[dict(source=f'return ({a}){op}({b})',mode=mode) for mode in ['default','safe','exact'] for a,b,op in itertools.product(vals,vals,ops)]
start=time.monotonic()
p=subprocess.run([sys.argv[1]],input=''.join(json.dumps(x)+'\n' for x in requests),text=True,capture_output=True,check=True)
assert not p.stderr,p.stderr
rows=[json.loads(x) for x in p.stdout.splitlines()]
assert len(rows)==len(requests),(len(rows),len(requests))
counts=collections.Counter();examples={};failures=[]
for req,row in zip(requests,rows):
 assert row['source']==req['source']
 if row.get('same'):kind='equal'
 elif 'panic' in row:kind='compiler_panic'
 elif 'compile_error' in row:kind='compiler_error'
 elif row['expected'].startswith('ERROR:') and row['actual'].startswith('ERROR:'):kind='both_runtime_error'
 elif row['expected'].startswith('ERROR:'):kind='removed_runtime_error'
 elif row['actual'].startswith('ERROR:'):kind='introduced_runtime_error'
 else:kind='different_values_or_subtypes'
 counts[(row['mode'],kind)]+=1
 if kind not in ('equal','both_runtime_error'):
  failures.append({k:v for k,v in row.items() if k!='passes'})
  key=(row['mode'],kind)
  if len(examples.setdefault(key,[]))<15:examples[key].append(failures[-1])
Path('matrix-failures.jsonl').write_text(''.join(json.dumps(x)+'\n' for x in failures))
summary={'cases':len(rows),'values':vals,'operators':ops,'modes':['default','safe','exact'],'elapsed_seconds':time.monotonic()-start,'counts':{f'{a}/{b}':n for (a,b),n in counts.items()},'examples':{f'{a}/{b}':x for (a,b),x in examples.items()},'notes':['Value/subtype differences are raw observations, not all actionable bugs; Smallest permits integer/float subtype changes.','Both runtime errors grouped without comparing stack-line offsets.']}
Path('matrix-summary.json').write_text(json.dumps(summary,indent=2))
print(json.dumps({k:v for k,v in summary.items() if k not in ['examples','values','operators']},indent=2))
