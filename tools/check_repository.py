#!/usr/bin/env python3
"""すべてのCargo依存宣言、生成されたドキュメントリンク、およびパブリックドキュメントの移植性を検証します。"""
import json
import re
import subprocess
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
IGNORED = {".git", ".worktrees", "node_modules", "target", "dist", "__pycache__", "artifacts"}

def validate_architecture(metadata, policy):
    members = set(metadata["workspace_members"])
    packages = {p["name"]: p for p in metadata["packages"] if p["id"] in members}
    if set(packages) != set(policy):
        raise ValueError("workspace membership does not match architecture policy")
    edges = {name: set() for name in packages}
    for name, package in packages.items():
        for dep in package["dependencies"]:
            target = dep["name"]  # 依存関係のエイリアスではなく、Cargoの実際のパッケージ名。
            kind = dep.get("kind") or "normal"
            if kind not in ("normal", "dev"):
                raise ValueError(f"build dependency not approved: {name} -> {target}")
            if target in packages:
                expected = Path(packages[target]["manifest_path"]).parent.resolve()
                if not dep.get("path") or Path(dep["path"]).resolve() != expected:
                    raise ValueError(f"workspace name resolves outside its owner: {name} -> {target}")
                if target not in policy[name][kind]:
                    raise ValueError(f"forbidden {kind} edge: {name} -> {target}")
                if policy[target]["role"] in ("test", "tool"):
                    raise ValueError("test/tool crates cannot be dependency targets")
                edges[name].add(target)
            elif target not in policy[name]["external"] or dep.get("path"):
                raise ValueError(f"forbidden external dependency: {name} -> {target}")
            allowed_features = policy[name].get("features", {}).get(target, [])
            unregistered = sorted(set(dep.get("features") or []) - set(allowed_features))
            if unregistered:
                raise ValueError(
                    f"unregistered feature activation: {name} -> {target}: {unregistered}")
    active, done = set(), set()
    def visit(name):
        if name in active: raise ValueError("dependency cycle, including dev edges")
        if name in done: return
        active.add(name)
        for target in edges[name]: visit(target)
        active.remove(name); done.add(name)
    for name in edges: visit(name)
    return packages

def files(root):
    for path in root.iterdir():
        if path.name in IGNORED or path.name.startswith("local-") or path.is_symlink(): continue
        if path.is_dir(): yield from files(path)
        else: yield path

def validate_target_graphs(policy):
    # 対象ターゲット向けの解決済みグラフに、登録された推移的禁止
    # パッケージ（lua-src 等の C ビルド依存）が到達していないことを
    # 検査する。直接宣言しか見ない validate_architecture の補完。
    checked = 0
    for name, entry in policy.items():
        graph = entry.get("target_graph")
        if not graph:
            continue
        # --offline は付けない: 対象ターゲット向けの解決には、その
        # ターゲットで初めて現れるクレートのマニフェストが要ることが
        # あり、CI のようにキャッシュが冷たい環境では取得できないと
        # 検査自体が成立しない（--locked で解決結果は固定される）。
        try:
            output = subprocess.check_output(
                ["cargo", "tree", "-p", name, "--target", graph["target"],
                 "--edges", "normal,build", "--prefix", "none",
                 "--locked"],
                cwd=ROOT, text=True, stderr=subprocess.STDOUT)
        except subprocess.CalledProcessError as error:
            raise ValueError(
                f"target graph resolution failed for {name} "
                f"({graph['target']}): {error.output.strip()}") from error
        present = {line.split(" ", 1)[0] for line in output.splitlines() if line.strip()}
        hits = sorted(present & set(graph["forbidden_transitive"]))
        if hits:
            raise ValueError(
                f"forbidden transitive dependency for {graph['target']}: {name} reaches {hits}")
        checked += 1
    return checked

def validate_docs():
    # プライベートなアプリケーション名はパブリックアーキテクチャの依存関係ではありません。
    forbidden = ("internal-docs.", "/home/", "/Users/", "~/.local/")
    count = 0
    for path in files(ROOT):
        if path.suffix != ".md": continue
        text = path.read_text(encoding="utf-8")
        if any(word.casefold() in text.casefold() for word in forbidden):
            raise ValueError(f"private/environment-specific reference: {path.relative_to(ROOT)}")
        for target in re.findall(r"\[[^\]]*\]\(([^)\s]+)\)", text):
            if "://" in target or target.startswith(("#", "mailto:")): continue
            target = target.split("#", 1)[0]
            if target and not (path.parent / target).exists():
                raise ValueError(f"broken local link in {path.relative_to(ROOT)}: {target}")
        count += 1
    if count < 5: raise ValueError("documentation set is unexpectedly empty")
    return count

def validate_notices():
    # 原文の所有者を変更せず、ソースとnpmに同じ許諾文を収録します。
    source = (ROOT / "licenses/screen-components-MIT.txt").read_bytes()
    packaged = (ROOT / "packages/lua-engine/SCREEN_COMPONENTS_LICENSE").read_bytes()
    if source != packaged or b"Copyright (c) 2026 Shannon-Toppo" not in source or b"MIT License" not in source:
        raise ValueError("screen component notices are missing or inconsistent")

def main():
    metadata = json.loads(subprocess.check_output(["cargo", "metadata", "--no-deps", "--format-version=1", "--locked", "--offline"], cwd=ROOT))
    policy = json.loads((ROOT / "tools/architecture.json").read_text())
    packages = validate_architecture(metadata, policy)
    graphs = validate_target_graphs(policy)
    count = validate_docs()
    from check_screen_fixtures import verify
    cases = verify()
    validate_notices()
    print(f"architecture: {len(packages)} classified crates, dependency ownership/DAG passed; {graphs} target graphs pure; {count} public Markdown files checked; {cases} accepted RGBA cases verified")

if __name__ == "__main__":
    main()
