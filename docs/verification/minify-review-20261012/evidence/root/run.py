#!/usr/bin/env python3
"""Build an external review consumer and replay JSONL cases against a chosen checkout."""
import argparse,json,os,shutil,subprocess,tempfile
from pathlib import Path
p=argparse.ArgumentParser();p.add_argument('--repo',type=Path,required=True);p.add_argument('--cases',type=Path);p.add_argument('--output',type=Path,required=True);p.add_argument('--release',action='store_true');a=p.parse_args()
repo=a.repo.resolve();here=Path(__file__).resolve().parent
with tempfile.TemporaryDirectory(prefix='minify-review-root-',dir='/tmp') as task:
 task=Path(task);(task/'src').mkdir();shutil.copy2(here/'harness.rs',task/'src/main.rs');shutil.copy2(here/'Cargo.lock',task/'Cargo.lock')
 deps='\n'.join(f'{name}={{path={json.dumps(str(repo/"crates"/name))}}}' for name in ['storm-lua-syntax','storm-lua-build','storm-lua-minify'])
 (task/'Cargo.toml').write_text('[package]\nname="minify-review-root"\nversion="0.1.0"\nedition="2021"\n[dependencies]\nserde_json="1"\nmlua={version="=0.10.5",default-features=false,features=["lua53","vendored"]}\n'+deps+'\n')
 env=os.environ.copy();env.update(CARGO_TARGET_DIR='/tmp/cargo-targets/storm-lua-engine/minify-review-20261012',CARGO_PROFILE_DEV_DEBUG='0',CARGO_INCREMENTAL='0',CARGO_BUILD_JOBS='4')
 cmd=['cargo','build','--offline','--manifest-path',str(task/'Cargo.toml')]
 if a.release:cmd.append('--release')
 subprocess.run(cmd,cwd=repo,env=env,check=True)
 binary=Path(env['CARGO_TARGET_DIR'])/('release' if a.release else 'debug')/'minify-review-root'
 requests=[json.loads(l) for l in (a.cases or here/'cases.jsonl').read_text().split('\n') if l]
 result=subprocess.run([binary],input=''.join(json.dumps(q)+'\n' for q in requests),text=True,capture_output=True,check=True,timeout=180)
 responses=[json.loads(l) for l in result.stdout.split('\n') if l]
 if len(requests)!=len(responses):raise RuntimeError('response count mismatch')
 a.output.parent.mkdir(parents=True,exist_ok=True)
 a.output.write_text(''.join(json.dumps({'request':q,'response':r})+'\n' for q,r in zip(requests,responses)))
 print(json.dumps({'executions':len(responses),'output':str(a.output),'note':'These are observations, not a passing-equivalence count.'}))
