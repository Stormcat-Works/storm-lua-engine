import json,resource,subprocess,time,sys
from pathlib import Path
request=dict(source='return string.format("%01000000000d",1)',mode='safe')
def limits():
 resource.setrlimit(resource.RLIMIT_AS,(256*1024*1024,256*1024*1024))
 resource.setrlimit(resource.RLIMIT_CORE,(0,0))
start=time.monotonic()
p=subprocess.run([sys.argv[1]],input=json.dumps(request)+'\n',text=True,capture_output=True,preexec_fn=limits)
result=dict(request=request,address_space_limit_bytes=256*1024*1024,returncode=p.returncode,stdout=p.stdout,stderr=p.stderr,elapsed_seconds=time.monotonic()-start)
Path('format-resource-result.json').write_text(json.dumps(result,indent=2))
print(json.dumps(result,indent=2))
