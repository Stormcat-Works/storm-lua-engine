#!/usr/bin/env python3
import json,subprocess,resource,sys
from pathlib import Path
rows=[]
def no_core():resource.setrlimit(resource.RLIMIT_CORE,(0,0))
for n in [100,200,400,800,1600,3200,6400,12000]:
 source='return '+'('*n+'1'+')'*n
 try:
  r=subprocess.run([sys.argv[1]],input=json.dumps({'source':source,'options':{'targetSize':8192}})+'\n',text=True,capture_output=True,timeout=10,preexec_fn=no_core)
  row={'depth':n,'bytes':len(source),'rc':r.returncode,'stdout':r.stdout[:1000],'stderr':r.stderr[:1000]}
 except subprocess.TimeoutExpired:row={'depth':n,'timeout':10}
 rows.append(row)
 if row.get('rc',0)<0:break
Path(sys.argv[2]).write_text(json.dumps(rows,indent=2)+'\n')
