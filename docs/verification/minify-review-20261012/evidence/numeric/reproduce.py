#!/usr/bin/env python3
"""Build the external review consumer and replay persisted cases against a checkout."""
import argparse
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile

p = argparse.ArgumentParser()
p.add_argument('--repo', type=Path, required=True, help='Checkout of reviewed commit (or candidate fix).')
p.add_argument('--target', type=Path, default=Path('/tmp/cargo-targets/storm-lua-engine/minify-numeric-review'))
p.add_argument('--cases', type=Path)
p.add_argument('--output', type=Path, default=Path('numeric-rerun.jsonl'))
p.add_argument('--build-only', action='store_true')
a = p.parse_args()
artifact = Path(__file__).resolve().parent
repo = a.repo.resolve()
target = a.target.resolve()
if not target.is_relative_to('/tmp/cargo-targets'):
    p.error('--target must remain below /tmp/cargo-targets')
with tempfile.TemporaryDirectory(prefix='minify-numeric-review-') as temp:
    crate = Path(temp)
    (crate / 'src').mkdir()
    shutil.copyfile(artifact / 'harness.rs', crate / 'src/main.rs')
    shutil.copyfile(artifact / 'Cargo.lock', crate / 'Cargo.lock')
    minify = json.dumps(str(repo / 'crates/storm-lua-minify'))
    syntax = json.dumps(str(repo / 'crates/storm-lua-syntax'))
    (crate / 'Cargo.toml').write_text(f'''[package]
name = "minify-numeric-review"
version = "0.1.0"
edition = "2021"
[dependencies]
storm-lua-minify = {{ path = {minify} }}
storm-lua-syntax = {{ path = {syntax} }}
mlua = {{version="=0.10.5", default-features=false, features=["lua53","vendored"]}}
serde_json = "1"
''')
    env = dict(os.environ)
    env.update(CARGO_TARGET_DIR=str(target), CARGO_PROFILE_DEV_DEBUG='0', CARGO_PROFILE_TEST_DEBUG='0', CARGO_INCREMENTAL='0')
    subprocess.run(['cargo', 'build', '--manifest-path', str(crate / 'Cargo.toml'), '--locked', '--offline'], env=env, check=True)
    binary = target / 'debug/minify-numeric-review'
    if a.build_only:
        print(binary)
    else:
        cases = a.cases or artifact / 'focused-cases.jsonl'
        with cases.open() as source, a.output.open('w') as result:
            subprocess.run([str(binary)], stdin=source, stdout=result, check=True)
        print(a.output.resolve())
