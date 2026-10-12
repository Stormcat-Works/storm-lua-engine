#!/usr/bin/env python3
"""Build a temporary native audit consumer and replay recorded minify cases."""
import argparse,json,os,shutil,subprocess,sys,tempfile
from pathlib import Path
p=argparse.ArgumentParser()
p.add_argument('--repo',type=Path,required=True,help='Audited Lua Engine checkout')
p.add_argument('--cases',type=Path,default=Path(__file__).with_name('cases.jsonl'))
p.add_argument('--release',action='store_true')
p.add_argument('--target-dir',default='/tmp/cargo-targets/storm-lua-minify-review-portable')
a=p.parse_args();repo=a.repo.resolve();target=Path(a.target_dir).resolve()
if not str(target).startswith('/tmp/cargo-targets/'):
    raise SystemExit('target-dir must remain beneath /tmp/cargo-targets/')
with tempfile.TemporaryDirectory(prefix='minify-review-api-') as directory:
    d=Path(directory);(d/'src').mkdir();shutil.copyfile(Path(__file__).with_name('harness.rs'),d/'src/main.rs');shutil.copyfile(Path(__file__).with_name('Cargo.lock'),d/'Cargo.lock')
    manifest='[package]\nname="minify-review-api"\nversion="0.0.0"\nedition="2021"\n[dependencies]\n'
    for name in ['storm-lua-minify','storm-lua-build','storm-lua-syntax']:
        manifest+=name+'={path='+json.dumps(str(repo/'crates'/name))+'}\n'
    manifest+='mlua={version="=0.10.5",default-features=false,features=["lua53","vendored"]}\nserde_json="1"\n'
    (d/'Cargo.toml').write_text(manifest)
    env={**os.environ,'CARGO_TARGET_DIR':str(target),'CARGO_PROFILE_DEV_DEBUG':'0','CARGO_PROFILE_TEST_DEBUG':'0','CARGO_INCREMENTAL':'0'}
    command=['cargo','build','--manifest-path',str(d/'Cargo.toml'),'--locked','--offline']
    if a.release:command.append('--release')
    subprocess.run(command,env=env,check=True,stdout=sys.stderr)
    with a.cases.open() as source:
        subprocess.run([str(target/('release' if a.release else 'debug')/'minify-review-api')],stdin=source,check=True)
