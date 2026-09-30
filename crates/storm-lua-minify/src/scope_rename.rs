// Canonical scope-renaming optimization.
//
// 意味保存: 各識別子バインディングへ短縮名を割り当てて参照を書き換える。
// 決定性（NFR-2）のため freq 降順 → 宣言順 (decl asc) / 名前順の走査順序と、
// shortNames() の決定的生成順を保つ。tie-break は TS localeCompare を ASCII では
// バイト比較で再現する（識別子は ASCII 中心のため同値）。

use std::collections::{HashMap, HashSet};

use storm_lua_analysis::resolver::{
    api_roots, reserved, resolve, Binding, BindingKind, Resolution, ScopeId,
};
use storm_lua_syntax::ast::{Ast, Node, NodeId};

/// scopeRename の結果。
pub struct ScopeRenameResult {
    /// 名前を書き換えた Ast（root は構造変更なし、Name/SymbolId のみ更新）。
    pub ast: Ast,
    pub root: NodeId,
    /// `old#bid → 新名`、fixed でないもののみ。
    pub mapping: HashMap<String, String>,
}

/// shortNames(): 'a'..'z','A'..'Z','_' で始まり、以降は '+数字' を並べる。
/// TS と同一生成順: 長さ1 の全候補 → 長さ2 → 長さ3 …。reserved/apiRoot は yield 前に除外。
const SHORT_NAME_FIRST: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ_";
const SHORT_NAME_TAIL: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ_0123456789";

struct ShortNames {
    /// 現在の長さ（1 始まり）
    len: usize,
    /// 現在の長さ内での列挙位置
    idx: u64,
    /// 長さ >= 2 を走査済みら開始したか
    started_len2: bool,
}

impl ShortNames {
    #[inline]
    fn new() -> Self {
        Self {
            len: 1,
            idx: 0,
            started_len2: false,
        }
    }
    /// 長さ len の全候補数（先頭は chars、残りは tail。len==1 は chars のみ）
    #[inline]
    fn count_len(&self, len: usize) -> u64 {
        if len == 1 {
            return SHORT_NAME_FIRST.len() as u64;
        }
        (SHORT_NAME_FIRST.len() as u64) * (SHORT_NAME_TAIL.len() as u64).pow((len - 1) as u32)
    }
    fn render(&self, len: usize, idx: u64) -> String {
        // mixed-radix: 最上位桁は chars(53)、以降の桁は tail(63)。
        // 識別子アルファベットは ASCII 固定なので、静的 byte slice から直接構築する。
        let mut v = idx;
        let mut text = String::with_capacity(len);
        for pos in 0..len {
            let tail_places = (len - 1 - pos) as u32;
            let placeval = (SHORT_NAME_TAIL.len() as u64).pow(tail_places);
            let alphabet = if pos == 0 {
                SHORT_NAME_FIRST
            } else {
                SHORT_NAME_TAIL
            };
            let digit = (v / placeval) % alphabet.len() as u64;
            text.push(char::from(alphabet[digit as usize]));
            v %= placeval.max(1);
        }
        text
    }
    fn next_candidate(&mut self) -> String {
        if self.len == 1 && !self.started_len2 {
            let cur = self.render(1, self.idx);
            self.idx += 1;
            if self.idx >= SHORT_NAME_FIRST.len() as u64 {
                self.len = 2;
                self.idx = 0;
                self.started_len2 = true;
            }
            return cur;
        }
        let cur = self.render(self.len, self.idx);
        self.idx += 1;
        if self.idx >= self.count_len(self.len) {
            self.len += 1;
            self.idx = 0;
        }
        cur
    }
}

/// 次の予約語でない短縮名（TS generator shortNames 相当）。
struct ShortNameIter<'a>(&'a mut ShortNames);
impl<'a> ShortNameIter<'a> {
    fn next(&mut self) -> String {
        loop {
            let c = self.0.next_candidate();
            if !reserved(&c) && !api_roots(&c) {
                return c;
            }
        }
    }
}

fn intervals_overlap(a: &Binding, b: &Binding) -> bool {
    a.group == b.group || !(a.last < b.decl || b.last < a.decl)
}

/// JS `String.prototype.localeCompare`（デフォルト ICU collation）の再現。
/// 主比較は ASCII の `_ < 0..9 < a..z (case-insensitive)`、同 primary のとき
/// lowercase < uppercase。識別子は ASCII 中心のため、この近似で byte 一致する。
/// 検証: fixtures で TS scopeRename 出力と 105 件 byte 一致（scope_rename_parity.rs）。
fn locale_compare(a: &str, b: &str) -> std::cmp::Ordering {
    let ab = a.as_bytes();
    let bb = b.as_bytes();
    let n = ab.len().min(bb.len());
    for i in 0..n {
        let pa = primary_weight(ab[i]);
        let pb = primary_weight(bb[i]);
        if pa != pb {
            return pa.cmp(&pb);
        }
    }
    if ab.len() != bb.len() {
        return ab.len().cmp(&bb.len());
    }
    for i in 0..n {
        let ua = ab[i].is_ascii_uppercase();
        let ub = bb[i].is_ascii_uppercase();
        if ua != ub {
            return ua.cmp(&ub);
        }
    }
    std::cmp::Ordering::Equal
}

fn primary_weight(c: u8) -> u8 {
    match c {
        b'_' => 0,
        b'0'..=b'9' => 1 + (c - b'0'),
        b'a'..=b'z' => 11 + (c - b'a'),
        b'A'..=b'Z' => 11 + (c - b'A'),
        _ => 99,
    }
}

/// scope 0 のバインディング一覧（TS resolver.scopes.get(0).bindings の順）。
fn scope0_bindings(resolution: &Resolution) -> &[u32] {
    &resolution.scope(0).bindings
}

/// renameGlobals: fixed の名前を used に加え、scope0 のグローバルに短名を割り当てる。
type RenameNames = Vec<Option<String>>;

fn rename_globals(
    ast: &Ast,
    resolution: &Resolution,
    names: &mut RenameNames,
    used: &mut HashSet<String>,
) {
    for (i, b) in resolution.bindings.iter().enumerate() {
        if i == 0 {
            continue;
        }
        if b.fixed || (b.kind == BindingKind::Global && resolution.binding_write_counts[i] == 0) {
            used.insert(ast.strings.get(b.name).to_string());
        }
    }
    let mut globals: Vec<u32> = scope0_bindings(resolution)
        .iter()
        .copied()
        .filter(|id| resolution.binding(*id).kind == BindingKind::Global)
        .collect();
    // freq 降順 → name asc（TS localeCompare。case-insensitive primary で diff<I<P を再現）
    globals.sort_by(|a, b| {
        let ba = resolution.binding(*a);
        let bb = resolution.binding(*b);
        bb.freq
            .cmp(&ba.freq)
            .then_with(|| locale_compare(ast.strings.get(ba.name), ast.strings.get(bb.name)))
    });
    let mut gg = ShortNames::new();
    let mut git = ShortNameIter(&mut gg);
    for id in globals {
        let b = resolution.binding(id);
        if b.fixed || resolution.binding_write_counts[id as usize] == 0 {
            names[id as usize] = Some(ast.strings.get(b.name).to_string());
            continue;
        }
        let q = loop {
            let q = git.next();
            if !used.contains(&q) {
                break q;
            }
        };
        names[id as usize] = Some(q.clone());
        used.insert(q);
    }
}

/// 各 scope の部分木が参照する全 bid（自分の refs + 子の refs の和）。
fn get_subtree_refs(resolution: &Resolution) -> Vec<HashSet<u32>> {
    fn gather(sid: ScopeId, resolution: &Resolution, out: &mut [HashSet<u32>]) {
        let scope = resolution.scope(sid);
        let mut refs: HashSet<u32> = scope.refs.iter().copied().collect();
        for &child in &scope.children {
            gather(child, resolution, out);
            refs.extend(out[child as usize].iter().copied());
        }
        out[sid as usize] = refs;
    }
    let mut out = vec![HashSet::new(); resolution.scopes.len()];
    gather(0, resolution, &mut out);
    out
}

fn rename_nested_scope(
    sid: ScopeId,
    resolution: &Resolution,
    names: &mut RenameNames,
    subtree_refs: &[HashSet<u32>],
) {
    let sc = resolution.scope(sid);
    let mut local_ids: Vec<u32> = sc
        .bindings
        .iter()
        .copied()
        .filter(|id| resolution.binding(*id).kind != BindingKind::Global)
        .collect();
    // With no local bindings there is nothing to assign or forbid here.
    // Descendants still need their own capture-aware assignment.
    if local_ids.is_empty() {
        for &child in &sc.children {
            rename_nested_scope(child, resolution, names, subtree_refs);
        }
        return;
    }
    let local_set: HashSet<u32> = local_ids.iter().copied().collect();
    let mut external: Vec<u32> = subtree_refs[sid as usize].iter().copied().collect();
    external.retain(|id| !local_set.contains(id) && names[*id as usize].is_some());
    // ShortNameIter already filters Lua keywords, API roots, onTick and onDraw.
    // Captured outer-scope names are checked once when each candidate is created.

    // TS: localIds.sort(freq desc || decl asc)
    local_ids.sort_by(|a, b| {
        let ba = resolution.binding(*a);
        let bb = resolution.binding(*b);
        bb.freq.cmp(&ba.freq).then_with(|| ba.decl.cmp(&bb.decl))
    });
    // Index the same deterministic short-name sequence. A candidate's
    // capture restriction is scope-wide; only lifetime conflicts change for
    // each binding. Reuse a dense buffer instead of hashing names per binding.
    let mut assigned = Vec::<(u32, usize)>::with_capacity(local_ids.len());
    let mut generator = ShortNames::new();
    let mut candidates = ShortNameIter(&mut generator);
    let mut short_names = Vec::<(String, bool)>::new();
    let mut blocked = Vec::<bool>::new();
    for id in &local_ids {
        let b = resolution.binding(*id);
        blocked.fill(false);
        for &(other_id, candidate) in &assigned {
            if intervals_overlap(b, resolution.binding(other_id)) {
                blocked[candidate] = true;
            }
        }
        let mut index = 0;
        let q = loop {
            if index == short_names.len() {
                let name = candidates.next();
                let available = !external
                    .iter()
                    .any(|other| names[*other as usize].as_deref() == Some(name.as_str()));
                short_names.push((name, available));
                blocked.push(false);
            }
            let (name, available) = &short_names[index];
            if *available && !blocked[index] {
                break name.clone();
            }
            index += 1;
        };
        names[*id as usize] = Some(q);
        assigned.push((*id, index));
    }
    for &c in &sc.children {
        rename_nested_scope(c, resolution, names, subtree_refs);
    }
}

fn rename_top_locals(
    _ast: &Ast,
    resolution: &Resolution,
    names: &mut RenameNames,
    used: &mut HashSet<String>,
) {
    let mut top_locals: Vec<u32> = scope0_bindings(resolution)
        .iter()
        .copied()
        .filter(|id| resolution.binding(*id).kind != BindingKind::Global)
        .collect();
    if top_locals.is_empty() {
        return;
    }
    top_locals.sort_by(|a, b| {
        let ba = resolution.binding(*a);
        let bb = resolution.binding(*b);
        bb.freq.cmp(&ba.freq)
    });
    let mut gg = ShortNames::new();
    let mut git = ShortNameIter(&mut gg);
    let mut own: HashSet<String> = HashSet::new();
    for id in top_locals {
        let q = loop {
            let q = git.next();
            if !used.contains(&q) && !own.contains(&q) {
                break q;
            }
        };
        names[id as usize] = Some(q.clone());
        own.insert(q);
    }
}

/// applyRenames: 書き換え後 Ast を構築する（arena 版）。
/// node_bid / node_bids をもとに Name / 宣言名（Local/Localfunc/Forin 名、関数 ps）の
/// SymbolId を新名へ置換する。置換は in-place（NodeId 維持）。
fn apply_renames(
    source: &Ast,
    resolution: &Resolution,
    root: NodeId,
    names: &RenameNames,
) -> (Ast, NodeId) {
    let mut ast = source.clone();
    let mut stack = vec![root];
    let mut visited = vec![false; ast.nodes.len()];
    while let Some(id) = stack.pop() {
        if std::mem::replace(&mut visited[id as usize], true) {
            continue;
        }
        // Only symbols change. The arena is already cloned, so preserve its
        // owned nodes and declaration lists instead of copying them again.
        // Keep the original traversal/intern order and unreachable nodes.
        let origin = ast.nodes.capture_origin(id);
        match &mut ast.nodes[id as usize] {
            Node::Name(symbol) | Node::Localfunc(symbol, _) | Node::Fornum(symbol, ..) => {
                if let Some(bid) = resolution.node_bid[id as usize] {
                    if let Some(name) = names[bid as usize].as_ref() {
                        *symbol = ast.strings.intern(name);
                    }
                }
            }
            Node::Local(symbols, _) | Node::Forin(symbols, ..) | Node::Function(symbols, ..) => {
                if let Some(bids) = resolution.node_bids.get(id as usize) {
                    for (symbol, bid) in symbols.iter_mut().zip(bids) {
                        if let Some(name) = names[*bid as usize].as_ref() {
                            *symbol = ast.strings.intern(name);
                        }
                    }
                }
            }
            _ => {}
        }
        ast.nodes.finish_rename(id, origin);
        storm_lua_analysis::resolver::for_each_child(&ast, id, &mut |child| stack.push(child));
    }
    (ast, root)
}

/// scope_rename パスの公開エントリ（TS scopeRename）。
fn scope_rename_impl(ast: &Ast, root: NodeId, collect_mapping: bool) -> ScopeRenameResult {
    let resolution = resolve(ast, root);
    let mut names: RenameNames = vec![None; resolution.bindings.len()];
    let mut used: HashSet<String> = HashSet::new();

    rename_globals(ast, &resolution, &mut names, &mut used);
    let subtree_refs = get_subtree_refs(&resolution);

    rename_top_locals(ast, &resolution, &mut names, &mut used);
    for &c in &resolution.scope(0).children {
        rename_nested_scope(c, &resolution, &mut names, &subtree_refs);
    }

    let mut mapping: HashMap<String, String> = HashMap::new();
    if collect_mapping {
        for (i, b) in resolution.bindings.iter().enumerate() {
            if i == 0 {
                continue;
            }
            if let Some(name) = names[i].as_ref() {
                if !b.fixed {
                    mapping.insert(format!("{}#{}", ast.strings.get(b.name), i), name.clone());
                }
            }
        }
    }

    let (new_ast, new_root) = apply_renames(ast, &resolution, root, &names);
    ScopeRenameResult {
        ast: new_ast,
        root: new_root,
        mapping,
    }
}

/// Public compatibility path: retain the diagnostic old-binding -> short-name map.
pub fn scope_rename(ast: &Ast, root: NodeId) -> ScopeRenameResult {
    scope_rename_impl(ast, root, true)
}

/// Compiler hot path: internal callers only consume the rewritten AST/root, so
/// avoid formatting and allocating the diagnostic mapping on every trial.
pub(crate) fn scope_rename_fast(ast: &Ast, root: NodeId) -> ScopeRenameResult {
    scope_rename_impl(ast, root, false)
}

/// テスト用: 予約語を除外した短縮名を TS の shortNames() と同じ順序で列挙する。
pub fn short_names_debug() -> ShortNameIterStatic {
    ShortNameIterStatic::new()
}

pub struct ShortNameIterStatic {
    ss: ShortNames,
}

impl ShortNameIterStatic {
    fn new() -> Self {
        Self {
            ss: ShortNames::new(),
        }
    }
}

impl Iterator for ShortNameIterStatic {
    type Item = String;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            let candidate = self.ss.next_candidate();
            if !reserved(&candidate) && !api_roots(&candidate) {
                return Some(candidate);
            }
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod allocation_regression_tests {
    use super::*;

    const SCOPE_CASES: &[(&str, &str, &str)] = &[
        ("capture", "local x=1 function onTick()local y=x return function(z)return y+z end end", ""),
        ("shadow", "local x=1 function onTick(x)do local x=x+1 output.setNumber(1,x)end output.setNumber(2,x)end", ""),
        ("loop", "function onTick()local sum=0 for i=1,20 do sum=sum+i end output.setNumber(1,sum)end", ""),
    ];

    // Independent reference for the replaced per-binding hash-set algorithm.
    fn reference_nested_scope(
        sid: ScopeId,
        resolution: &Resolution,
        names: &mut RenameNames,
        subtree_refs: &[HashSet<u32>],
    ) {
        let sc = resolution.scope(sid);
        let mut local_ids: Vec<u32> = sc
            .bindings
            .iter()
            .copied()
            .filter(|id| resolution.binding(*id).kind != BindingKind::Global)
            .collect();
        // With no local bindings there is nothing to assign or forbid here.
        // Descendants still need their own capture-aware assignment.
        if local_ids.is_empty() {
            for &child in &sc.children {
                reference_nested_scope(child, resolution, names, subtree_refs);
            }
            return;
        }
        let local_set: HashSet<u32> = local_ids.iter().copied().collect();
        let mut external: Vec<u32> = subtree_refs[sid as usize].iter().copied().collect();
        external.retain(|id| !local_set.contains(id) && names[*id as usize].is_some());
        let mut forbidden: HashSet<String> = HashSet::with_capacity(external.len());
        for id in &external {
            forbidden.insert(names[*id as usize].clone().unwrap_or_default());
        }

        // TS: localIds.sort(freq desc || decl asc)
        local_ids.sort_by(|a, b| {
            let ba = resolution.binding(*a);
            let bb = resolution.binding(*b);
            bb.freq.cmp(&ba.freq).then_with(|| ba.decl.cmp(&bb.decl))
        });
        let mut assigned: Vec<u32> = Vec::new();
        // Every local restarts the same deterministic sequence. Format each name
        // once per scope instead of rebuilding the sequence for each binding.
        let mut generator = ShortNames::new();
        let mut candidates = ShortNameIter(&mut generator);
        let mut short_names = Vec::<String>::new();
        for id in &local_ids {
            let b = resolution.binding(*id);
            let mut blocked: HashSet<&str> = HashSet::new();
            for other_id in &assigned {
                if intervals_overlap(b, resolution.binding(*other_id)) {
                    blocked.insert(names[*other_id as usize].as_deref().unwrap_or_default());
                }
            }
            let mut index = 0;
            let q = loop {
                if index == short_names.len() {
                    short_names.push(candidates.next());
                }
                let q = &short_names[index];
                if !forbidden.contains(q) && !blocked.contains(q.as_str()) {
                    break q.clone();
                }
                index += 1;
            };
            names[*id as usize] = Some(q);
            assigned.push(*id);
        }
        for &c in &sc.children {
            reference_nested_scope(c, resolution, names, subtree_refs);
        }
    }

    fn assigned_names(ast: &Ast, root: NodeId, reference: bool) -> RenameNames {
        let resolution = resolve(ast, root);
        let mut names = vec![None; resolution.bindings.len()];
        let mut used = HashSet::new();
        rename_globals(ast, &resolution, &mut names, &mut used);
        rename_top_locals(ast, &resolution, &mut names, &mut used);
        let refs = get_subtree_refs(&resolution);
        for &child in &resolution.scope(0).children {
            if reference {
                reference_nested_scope(child, &resolution, &mut names, &refs);
            } else {
                rename_nested_scope(child, &resolution, &mut names, &refs);
            }
        }
        names
    }

    fn check(source: &str) {
        let (ast, root) = storm_lua_syntax::parser::parse_source(source).unwrap();
        assert_eq!(
            assigned_names(&ast, root, false),
            assigned_names(&ast, root, true)
        );
        let full = scope_rename(&ast, root);
        let fast = scope_rename_fast(&ast, root);
        assert_eq!(full.root, fast.root);
        assert!(full.ast.nodes == fast.ast.nodes);
        assert_eq!(
            full.ast.strings.all_strings(),
            fast.ast.strings.all_strings()
        );
        assert!(fast.mapping.is_empty());
    }

    #[test]
    fn dense_name_conflicts_and_mapping_free_path_match_synthetic_reference() {
        for (_, source, _) in SCOPE_CASES {
            check(source);
        }
    }

    #[test]
    fn dense_name_conflicts_preserve_reuse_captures_and_long_names() {
        check("outside=3 function onTick(p) do local a=p output.setNumber(1,a) end do local b=p+1 output.setNumber(2,b) end local x,y=p,2 local f=function(z) return x+y+z+outside end return f(p) end");
        for count in [1, 8, 52, 53, 54, 96, 160] {
            let params = (0..count)
                .map(|i| format!("parameter{i}"))
                .collect::<Vec<_>>();
            let source = format!(
                "external=1 function onTick({}) local captured=external+parameter0 do local external=7 output.setNumber(1,external) end return function(extra) return captured+extra+{} end end",
                params.join(","), params.join("+")
            );
            check(&source);
        }
    }
}
