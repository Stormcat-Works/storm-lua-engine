// Canonical scope-renaming optimization.
//
// 意味保存: 各識別子バインディングへ短縮名を割り当てて参照を書き換える。
// 決定性（NFR-2）のため freq 降順 → 宣言順 (decl asc) / 名前順の走査順序と、
// shortNames() の決定的生成順を保つ。tie-break は TS localeCompare を ASCII では
// バイト比較で再現する（識別子は ASCII 中心のため同値）。

use std::borrow::Cow;
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use storm_lua_analysis::resolver::{
    api_roots, reserved, resolve, Binding, BindingKind, Resolution, ScopeId,
};
use storm_lua_syntax::ast::{Ast, Node, NodeId};
use storm_lua_syntax::node_arena::RenameSharedTails;

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
const SHORT_NAME_FIRST_TEXT: &str = "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ_";
const SHORT_NAME_FIRST: &[u8] = SHORT_NAME_FIRST_TEXT.as_bytes();
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
    fn render(&self, len: usize, idx: u64) -> Cow<'static, str> {
        if len == 1 {
            let offset = idx as usize;
            return Cow::Borrowed(&SHORT_NAME_FIRST_TEXT[offset..offset + 1]);
        }
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
        Cow::Owned(text)
    }
    fn next_candidate(&mut self) -> Cow<'static, str> {
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
    fn next(&mut self) -> Cow<'static, str> {
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
pub(crate) type RenameNames<'source> = Vec<Option<Cow<'source, str>>>;

fn rename_globals<'source>(
    ast: &'source Ast,
    resolution: &Resolution,
    names: &mut RenameNames<'source>,
    used: &mut HashSet<Cow<'source, str>>,
    globals_fixed: bool,
) {
    if globals_fixed {
        // Local-only renaming (lexical winners): every global keeps its
        // original spelling, because a global can be observed through _ENV.
        // Fixed non-global bindings (none exist today: `fixed` is set only
        // for reserved/api-root globals) still reserve their spelling in
        // `used` so top-level locals cannot take it.
        for (i, b) in resolution.bindings.iter().enumerate() {
            if i == 0 {
                continue;
            }
            if b.kind == BindingKind::Global {
                names[i] = Some(Cow::Borrowed(ast.strings.get(b.name)));
                used.insert(Cow::Borrowed(ast.strings.get(b.name)));
            } else if b.fixed {
                used.insert(Cow::Borrowed(ast.strings.get(b.name)));
            }
        }
        return;
    }
    for (i, b) in resolution.bindings.iter().enumerate() {
        if i == 0 {
            continue;
        }
        if b.fixed || (b.kind == BindingKind::Global && resolution.binding_write_counts[i] == 0) {
            used.insert(Cow::Borrowed(ast.strings.get(b.name)));
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
            names[id as usize] = Some(Cow::Borrowed(ast.strings.get(b.name)));
            continue;
        }
        let q = loop {
            let q = git.next();
            if !used.contains(q.as_ref()) {
                break q;
            }
        };
        names[id as usize] = Some(q.clone());
        used.insert(q);
    }
}

/// 各 scope の部分木が参照する全 bid（自分の refs + 子の refs の和）。
fn get_subtree_refs(resolution: &Resolution) -> Vec<Vec<u32>> {
    fn gather(sid: ScopeId, resolution: &Resolution, out: &mut [Vec<u32>]) {
        let scope = resolution.scope(sid);
        let mut refs = scope.refs.clone();
        for &child in &scope.children {
            gather(child, resolution, out);
            refs.extend_from_slice(&out[child as usize]);
        }
        // Consumers require membership, not randomized hash iteration order.
        refs.sort_unstable();
        refs.dedup();
        out[sid as usize] = refs;
    }
    let mut out = vec![Vec::new(); resolution.scopes.len()];
    gather(0, resolution, &mut out);
    out
}

fn rename_nested_scope<'a>(
    ast: &'a Ast,
    sid: ScopeId,
    resolution: &Resolution,
    names: &mut RenameNames<'a>,
    subtree_refs: &[Vec<u32>],
    preserve_env: bool,
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
            rename_nested_scope(ast, child, resolution, names, subtree_refs, preserve_env);
        }
        return;
    }
    let mut local_set = local_ids.clone();
    local_set.sort_unstable();
    let external = subtree_refs[sid as usize]
        .iter()
        .copied()
        .filter(|id| local_set.binary_search(id).is_err() && names[*id as usize].is_some())
        .collect::<Vec<_>>();
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
    let mut short_names = Vec::<(Cow<'static, str>, bool)>::new();
    let mut blocked = Vec::<bool>::new();
    for id in &local_ids {
        let b = resolution.binding(*id);
        if preserve_env && ast.strings.get(b.name) == "_ENV" {
            // A local shadowing _ENV is the most delicate case: keep its
            // original spelling instead of renaming it.
            names[*id as usize] = Some(Cow::Borrowed(ast.strings.get(b.name)));
            continue;
        }
        blocked.fill(false);
        for &(other_id, candidate) in &assigned {
            if intervals_overlap(b, resolution.binding(other_id)) {
                blocked[candidate] = true;
            }
        }
        let mut index = 0;
        let q = loop {
            if index == short_names.len() {
                // Never assign _ENV as a new name: that would capture the
                // global environment table instead of a local.
                let name = loop {
                    let name = candidates.next();
                    if !preserve_env || name != "_ENV" {
                        break name;
                    }
                };
                let available = !external
                    .iter()
                    .any(|other| names[*other as usize].as_deref() == Some(name.as_ref()));
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
        rename_nested_scope(ast, c, resolution, names, subtree_refs, preserve_env);
    }
}

fn rename_top_locals<'a>(
    ast: &'a Ast,
    resolution: &Resolution,
    names: &mut RenameNames<'a>,
    used: &mut HashSet<Cow<'_, str>>,
    preserve_env: bool,
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
    // This generator yields each spelling once. Earlier local assignments
    // cannot reappear, so another hash set of the same unique sequence is not
    // needed; globally reserved spellings still use the shared used set.
    for id in top_locals {
        let b = resolution.binding(id);
        if preserve_env && ast.strings.get(b.name) == "_ENV" {
            names[id as usize] = Some(Cow::Borrowed(ast.strings.get(b.name)));
            continue;
        }
        let q = loop {
            let q = git.next();
            if preserve_env && q == "_ENV" {
                continue;
            }
            if !used.contains(q.as_ref()) {
                break q;
            }
        };
        names[id as usize] = Some(q);
    }
}

/// applyRenames: 書き換え後 Ast を構築する（arena 版）。
/// node_bid / node_bids をもとに Name / 宣言名（Local/Localfunc/Forin 名、関数 ps）の
/// SymbolId を新名へ置換する。置換は in-place（NodeId 維持）。
/// Shared rename application: the working arena is read for its own
/// rename decisions, which is sound because a node is visited once and
/// only its own symbols are rewritten at its visit, so no decision reads
/// a value written by an earlier visit. Child IDs never change, so the
/// traversal from the working arena matches the source traversal.
/// Callers that must keep their input unchanged pass a clone; callers
/// that own their arena pass it directly, which keeps uniquely owned
/// origin pages detached instead of paying a page copy per touched page.
fn apply_renames_inner(
    mut ast: Ast,
    resolution: &Resolution,
    root: NodeId,
    names: &RenameNames<'_>,
) -> (Ast, NodeId) {
    // One transformation code string for every renamed node in this pass.
    // `finish_rename` would otherwise allocate a fresh `Arc<str>` per origin
    // and per name entry; the recorded origins are identical either way.
    let rename_code: Arc<str> = Arc::from("scope-renaming");
    // One shared reason-tail node per derivation, built only when origins
    // are tracked, so a batch rename allocates them once instead of once
    // per renamed origin. Untracked compilation allocates nothing extra.
    let shared_tails = ast
        .nodes
        .tracks_origins()
        .then(|| RenameSharedTails::new(&rename_code));
    let mut stack = vec![root];
    let mut visited = vec![false; ast.nodes.len()];
    while let Some(id) = stack.pop() {
        if std::mem::replace(&mut visited[id as usize], true) {
            continue;
        }
        // Determine renames through immutable access first. Mutable indexing of a
        // tracked NodeArena deliberately invalidates provenance, so touching every
        // non-name node would force a capture/restore allocation for no semantic
        // change. Only mutate nodes whose emitted identifier spelling changes.
        let mut single = None::<&str>;
        let mut multiple = Vec::<(usize, &str)>::new();
        match ast.node(id) {
            Node::Name(symbol) | Node::Localfunc(symbol, _) | Node::Fornum(symbol, ..) => {
                if let Some(bid) = resolution.node_bid[id as usize] {
                    if let Some(name) = names[bid as usize].as_deref() {
                        if ast.strings.get(*symbol) != name {
                            single = Some(name);
                        }
                    }
                }
            }
            Node::Local(symbols, _) | Node::Forin(symbols, ..) | Node::Function(symbols, ..) => {
                if let Some(bids) = resolution.node_bids.get(id as usize) {
                    for (index, (symbol, bid)) in symbols.iter().zip(bids).enumerate() {
                        if let Some(name) = names[*bid as usize].as_deref() {
                            if ast.strings.get(*symbol) != name {
                                multiple.push((index, name));
                            }
                        }
                    }
                }
            }
            _ => {}
        }
        if let Some(name) = single {
            let replacement = ast.strings.intern(name);
            // The spelling differs by construction, so the node value
            // changed: use the borrowed finish, which skips the snapshot's
            // node clone (multi-symbol nodes copy several vectors there).
            let origin = ast.nodes.capture_origin_ref(id);
            match &mut ast.nodes[id as usize] {
                Node::Name(symbol) | Node::Localfunc(symbol, _) | Node::Fornum(symbol, ..) => {
                    *symbol = replacement;
                }
                _ => unreachable!("single rename action changed node shape"),
            }
            if let Some(origin) = origin {
                ast.nodes
                    .finish_rename_changed(id, origin, &rename_code, shared_tails.as_ref());
            }
        } else if !multiple.is_empty() {
            let replacements = multiple
                .into_iter()
                .map(|(index, name)| (index, ast.strings.intern(name)))
                .collect::<Vec<_>>();
            // At least one symbol differs by construction, so the node
            // value changed: same borrowed finish as the single case.
            let origin = ast.nodes.capture_origin_ref(id);
            match &mut ast.nodes[id as usize] {
                Node::Local(symbols, _)
                | Node::Forin(symbols, ..)
                | Node::Function(symbols, ..) => {
                    for (index, replacement) in replacements {
                        symbols[index] = replacement;
                    }
                }
                _ => unreachable!("multi rename action changed node shape"),
            }
            if let Some(origin) = origin {
                ast.nodes
                    .finish_rename_changed(id, origin, &rename_code, shared_tails.as_ref());
            }
        }
        storm_lua_analysis::resolver::for_each_child(&ast, id, &mut |child| stack.push(child));
    }
    (ast, root)
}

/// scope_rename パスの公開エントリ（TS scopeRename）。
/// Bindings named `self` are never renamed. The AST does not bind the
/// implicit method parameter, so occurrences of the name inside a method
/// body resolve to an enclosing `self` binding even though they denote
/// the receiver. Renaming the binding rewrites those occurrences along
/// with the genuine ones, but the printed method keeps its colon sugar,
/// whose implicit parameter is spelled `self` -- a renamed occurrence
/// would read the enclosing binding instead of the receiver
/// (candidate AA, w9 cycle 68). Keeping the binding's spelling keeps
/// both the genuine and the receiver occurrences correct.
fn suppress_self_renames(ast: &Ast, resolution: &Resolution, names: &mut RenameNames<'_>) {
    // Compare symbol ids, not strings: a rename plan is also computed
    // for size measurement over trial arenas whose identifier registry
    // may not hold every id, where `Interner::get` on an arbitrary
    // binding name is not safe. If this registry has no `self` symbol,
    // no binding in it can be spelled `self` either.
    let Some(self_id) = ast
        .strings
        .iter()
        .position(|name| name == "self")
        .map(|index| storm_lua_syntax::ast::SymbolId(index as u32))
    else {
        return;
    };
    for (i, binding) in resolution.bindings.iter().enumerate() {
        if binding.name == self_id {
            names[i] = None;
        }
    }
}

fn rename_plan(ast: &Ast, root: NodeId) -> (Resolution, RenameNames<'_>) {
    let resolution = resolve(ast, root);
    let mut names: RenameNames<'_> = vec![None; resolution.bindings.len()];
    let mut used: HashSet<Cow<'_, str>> = HashSet::new();

    rename_globals(ast, &resolution, &mut names, &mut used, false);
    let subtree_refs = get_subtree_refs(&resolution);
    rename_top_locals(ast, &resolution, &mut names, &mut used, false);
    for &c in &resolution.scope(0).children {
        rename_nested_scope(ast, c, &resolution, &mut names, &subtree_refs, false);
    }
    suppress_self_renames(ast, &resolution, &mut names);
    (resolution, names)
}

/// Local-only rename plan for lexical winners: globals keep their original
/// spelling (they can be observed through _ENV), locals are renamed with
/// the same ordering, lifetime-reuse and capture-blocking rules as the
/// full plan, except that `_ENV` is never assigned as a new name and a
/// local originally named `_ENV` keeps its spelling. This is the plan
/// whose corpus gain was censused as ~178,785 characters on the five
/// lexical winners (globals-fixed variant of renamecensus2).
pub(crate) fn local_only_rename_plan<'source>(
    ast: &'source Ast,
    root: NodeId,
) -> (Resolution, RenameNames<'source>) {
    let resolution = resolve(ast, root);
    let mut names: RenameNames<'source> = vec![None; resolution.bindings.len()];
    let mut used: HashSet<Cow<'source, str>> = HashSet::new();
    rename_globals(ast, &resolution, &mut names, &mut used, true);
    let subtree_refs = get_subtree_refs(&resolution);
    rename_top_locals(ast, &resolution, &mut names, &mut used, true);
    for &c in &resolution.scope(0).children {
        rename_nested_scope(ast, c, &resolution, &mut names, &subtree_refs, true);
    }
    suppress_self_renames(ast, &resolution, &mut names);
    (resolution, names)
}

fn scope_rename_impl(ast: &Ast, root: NodeId, collect_mapping: bool) -> ScopeRenameResult {
    let (resolution, names) = rename_plan(ast, root);

    let mut mapping: HashMap<String, String> = HashMap::new();
    if collect_mapping {
        for (i, b) in resolution.bindings.iter().enumerate() {
            if i == 0 {
                continue;
            }
            if let Some(name) = names[i].as_ref() {
                if !b.fixed {
                    mapping.insert(
                        format!("{}#{}", ast.strings.get(b.name), i),
                        name.to_string(),
                    );
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

fn apply_renames(
    source: &Ast,
    resolution: &Resolution,
    root: NodeId,
    names: &RenameNames<'_>,
) -> (Ast, NodeId) {
    apply_renames_inner(source.clone(), resolution, root, names)
}

/// Compiler hot path for callers that own their arena (for example the
/// api-alias winner, whose origin pages are freshly attached and uniquely
/// owned): applying the rename in place avoids the clone, whose shared
/// origin pages would otherwise be copied once per touched page on the
/// first tracked write. The resulting AST, roots, origins and printed
/// code are identical to the fast path on the same input.
pub(crate) fn scope_rename_fast_owned(ast: Ast, root: NodeId) -> ScopeRenameResult {
    // The rename plan depends only on syntax and strings, never on
    // provenance, so it is taken from an origins-free clone: its name
    // table borrows that clone (kept alive through the application),
    // while the working arena itself can be moved into the application
    // without a borrow conflict. The clone shares no origin pages with
    // the working arena, so a uniquely owned working arena stays
    // detached during the application.
    let plan_source = storm_lua_syntax::ast_utils::clone_without_origins(&ast);
    let (resolution, names) = rename_plan(&plan_source, root);
    let (new_ast, new_root) = apply_renames_inner(ast, &resolution, root, &names);
    ScopeRenameResult {
        ast: new_ast,
        root: new_root,
        mapping: HashMap::new(),
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
                return Some(candidate.into_owned());
            }
        }
    }
}

/// Measure the scope-renamed size without building provenance for a disposable
/// cost-model AST. The actual selected scope rename still receives the tracked AST.
struct RenameSizeResolver<'a> {
    resolution: &'a Resolution,
    names: &'a RenameNames<'a>,
}

impl storm_lua_syntax::size::SizeNameResolver for RenameSizeResolver<'_> {
    fn replacement(&self, node: NodeId, site: storm_lua_syntax::parser::NameSite) -> Option<&str> {
        use storm_lua_syntax::parser::NameSite;
        let bid = match site {
            NameSite::Reference => self
                .resolution
                .node_bid
                .get(node as usize)
                .copied()
                .flatten(),
            NameSite::Binding(index) | NameSite::Parameter(index) => self
                .resolution
                .node_bids
                .get(node as usize)
                .and_then(|bids| bids.get(index as usize))
                .copied()
                .or_else(|| {
                    (index == 0)
                        .then(|| {
                            self.resolution
                                .node_bid
                                .get(node as usize)
                                .copied()
                                .flatten()
                        })
                        .flatten()
                }),
            NameSite::Field(_) | NameSite::Member | NameSite::Label => None,
        }?;
        self.names.get(bid as usize)?.as_deref()
    }
}

pub(crate) fn measure_renamed_size(ast: &Ast, root: NodeId) -> usize {
    if let Some(size) = renamed_size_memo_get(ast, root) {
        return size;
    }
    let size = compute_renamed_size(ast, root);
    renamed_size_memo_put(ast, root, size);
    size
}

/// `measure_renamed_size` without the memo: always recomputes, and never
/// reads or writes the memo.
///
/// The value is identical to `measure_renamed_size` for every input. The
/// measurement is a pure function of the subtree syntax and the strings
/// it references, and the memo is value-transparent: a hit returns
/// exactly what recomputation would produce for the same arena stamp
/// (see the memo's documentation), so the sequence of measured values
/// cannot depend on which calls go through the memo. Skipping it
/// changes only recomputation cost, never a measured size or a
/// selection built on sizes.
///
/// The reason to skip it is the memo's one side effect that reaches
/// outside the memo itself: an inserted entry retains `Arc` clones of
/// the measured buffers, keeping a one-shot buffer shared, so a later
/// mutation of that buffer pays a full copy-on-write detach. Callers
/// that recycle a trial arena across measurements (signed-factoring's
/// size trials) measure through this route so the recycled buffer
/// stays uniquely owned and restoring it stays detach-free.
pub(crate) fn measure_renamed_size_unmemoized(ast: &Ast, root: NodeId) -> usize {
    compute_renamed_size(ast, root)
}

/// Print a scope-renamed rendering of a tree WITHOUT materializing the
/// rename: compute the rename plan on this tree and print with the plan
/// as a spelling substitution (mapping-aware print).
///
/// The output must equal the stock print of the renamed tree
/// (`scope_rename_fast(ast, root)` then print) for every input: the
/// printer substitutes exactly the spellings the rename would install
/// (same plan, same bid lookup), and local-merge decisions compare
/// post-substitution spellings, matching the renamed tree's decisions.
/// This is the measurement primitive for deferred rename: a loser's
/// exact renamed size is `print_with_rename_mapping(...).len()` with no
/// arena detach. It changes no selection or output by itself; it is
/// only a measurement until a caller wires it into finalize.
/// The deferred two-phase design's caller lands in a later cycle; until
/// then this measurement primitive is covered by its unit test and by
/// the corpus-wide print-equivalence gate, not by a production caller.
#[allow(dead_code)]
pub(crate) fn print_with_rename_mapping(
    ast: &Ast,
    root: NodeId,
    zero_cost_newlines: bool,
) -> String {
    let (resolution, names) = rename_plan(ast, root);
    let resolver = RenameSizeResolver {
        resolution: &resolution,
        names: &names,
    };
    let mut printer =
        storm_lua_syntax::Printer::with_name_resolver(ast, zero_cost_newlines, &resolver);
    printer.output(root)
}

fn compute_renamed_size(ast: &Ast, root: NodeId) -> usize {
    let (resolution, names) = rename_plan(ast, root);
    let resolver = RenameSizeResolver {
        resolution: &resolution,
        names: &names,
    };
    storm_lua_syntax::size::measure_size_with_names(ast, root, &resolver)
}

/// Memoized `measure_renamed_size` results, keyed by arena identity.
///
/// The measurement is a pure function of the subtree syntax and the
/// identifier strings it references: the rename plan reads strings only
/// through `Interner::get` for ids reachable from the subtree, and the
/// interner is append-only, so an existing id's string never changes
/// within one strings buffer. An entry therefore stays valid exactly
/// while (values buffer, arena generation, strings buffer, symbol count)
/// is unchanged: the arena generation advances on every syntax mutation
/// (see `NodeArena::values_mut`), and any interner growth advances the
/// symbol count. Entries hold both buffer `Arc`s, so a key's addresses
/// cannot be recycled by a different buffer while the entry lives.
/// Bounds: at most `RENAMED_SIZE_MEMO_MAX_BUFFERS` buffer pairs and
/// `RENAMED_SIZE_MEMO_MAX_SIZES` total sizes; exceeding either clears
/// the whole memo (a compile uses a handful of live arenas at a time).
struct RenamedSizeEntry {
    values: Arc<Vec<Node>>,
    strings: Arc<Vec<Arc<str>>>,
    generation: u64,
    symbol_count: usize,
    sizes: HashMap<NodeId, usize>,
}

#[derive(Default)]
struct RenamedSizeMemo {
    entries: HashMap<(usize, usize), RenamedSizeEntry>,
    total_sizes: usize,
}

const RENAMED_SIZE_MEMO_MAX_BUFFERS: usize = 64;
const RENAMED_SIZE_MEMO_MAX_SIZES: usize = 32768;

thread_local! {
    static RENAMED_SIZE_MEMO: RefCell<RenamedSizeMemo> = RefCell::new(RenamedSizeMemo::default());
}

fn renamed_size_memo_get(ast: &Ast, root: NodeId) -> Option<usize> {
    let values = ast.nodes.values_arc();
    let strings = ast.strings.strings_arc();
    let key = (
        Arc::as_ptr(&values) as usize,
        Arc::as_ptr(&strings) as usize,
    );
    RENAMED_SIZE_MEMO.with(|memo| {
        let mut memo = memo.borrow_mut();
        let (hit, cleared) = {
            let entry = memo.entries.get_mut(&key)?;
            if !Arc::ptr_eq(&entry.values, &values) || !Arc::ptr_eq(&entry.strings, &strings) {
                return None;
            }
            let mut cleared = 0;
            if entry.generation != ast.nodes.generation()
                || entry.symbol_count != ast.strings.symbol_count()
            {
                cleared = entry.sizes.len();
                entry.sizes.clear();
                entry.generation = ast.nodes.generation();
                entry.symbol_count = ast.strings.symbol_count();
            }
            (entry.sizes.get(&root).copied(), cleared)
        };
        memo.total_sizes -= cleared;
        hit
    })
}

fn renamed_size_memo_put(ast: &Ast, root: NodeId, size: usize) {
    let values = ast.nodes.values_arc();
    let strings = ast.strings.strings_arc();
    let key = (
        Arc::as_ptr(&values) as usize,
        Arc::as_ptr(&strings) as usize,
    );
    let generation = ast.nodes.generation();
    let symbol_count = ast.strings.symbol_count();
    RENAMED_SIZE_MEMO.with(|memo| {
        let mut memo = memo.borrow_mut();
        if memo.total_sizes >= RENAMED_SIZE_MEMO_MAX_SIZES
            || (!memo.entries.contains_key(&key)
                && memo.entries.len() >= RENAMED_SIZE_MEMO_MAX_BUFFERS)
        {
            memo.entries.clear();
            memo.total_sizes = 0;
        }
        let (cleared, added) = {
            let entry = memo.entries.entry(key).or_insert_with(|| RenamedSizeEntry {
                values: Arc::clone(&values),
                strings: Arc::clone(&strings),
                generation,
                symbol_count,
                sizes: HashMap::new(),
            });
            let mut cleared = 0;
            if !Arc::ptr_eq(&entry.values, &values)
                || !Arc::ptr_eq(&entry.strings, &strings)
                || entry.generation != generation
                || entry.symbol_count != symbol_count
            {
                cleared = entry.sizes.len();
                entry.values = Arc::clone(&values);
                entry.strings = Arc::clone(&strings);
                entry.generation = generation;
                entry.symbol_count = symbol_count;
                entry.sizes.clear();
            }
            let added = entry.sizes.insert(root, size).is_none();
            (cleared, added)
        };
        memo.total_sizes = memo.total_sizes - cleared + usize::from(added);
    });
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
        names: &mut RenameNames<'_>,
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
            forbidden.insert(names[*id as usize].clone().unwrap_or_default().into_owned());
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
                    short_names.push(candidates.next().into_owned());
                }
                let q = &short_names[index];
                if !forbidden.contains(q) && !blocked.contains(q.as_str()) {
                    break q.clone();
                }
                index += 1;
            };
            names[*id as usize] = Some(Cow::Owned(q));
            assigned.push(*id);
        }
        for &c in &sc.children {
            reference_nested_scope(c, resolution, names, subtree_refs);
        }
    }

    fn assigned_names(ast: &Ast, root: NodeId, reference: bool) -> RenameNames<'_> {
        let resolution = resolve(ast, root);
        let mut names = vec![None; resolution.bindings.len()];
        let mut used = HashSet::new();
        rename_globals(ast, &resolution, &mut names, &mut used, false);
        rename_top_locals(ast, &resolution, &mut names, &mut used, false);
        let refs = get_subtree_refs(&resolution);
        let legacy_refs = refs
            .iter()
            .map(|r| r.iter().copied().collect::<HashSet<_>>())
            .collect::<Vec<_>>();
        for &child in &resolution.scope(0).children {
            if reference {
                reference_nested_scope(child, &resolution, &mut names, &legacy_refs);
            } else {
                rename_nested_scope(ast, child, &resolution, &mut names, &refs, false);
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
    fn self_bindings_are_never_renamed() {
        // Candidate AA (w9 cycle 68): a binding named `self` keeps its
        // spelling, because method-body occurrences that resolve to it
        // denote the receiver and must keep reading the implicit
        // parameter. Other locals are still renamed.
        let source = "local self = {x = 111} local otherlongname = 2 local T = {x = 222} function T:m() return self.x + otherlongname end function onTick() output.setNumber(1, T:m()) end";
        let (ast, root) = storm_lua_syntax::parser::parse_source(source).unwrap();
        let full = scope_rename(&ast, root);
        assert!(
            full.mapping.keys().all(|k| !k.starts_with("self#")),
            "{:?}",
            full.mapping
        );
        assert!(!full.mapping.is_empty(), "{:?}", full.mapping);
    }

    #[test]
    fn owned_fast_rename_matches_borrowed_fast_rename() {
        let source = "local descriptive=1 function onTick()local other=descriptive+1 output.setNumber(1,other)end";
        let (ast, root) =
            storm_lua_syntax::parser::parse_source_with_origins("owned.lua", source).unwrap();
        let expected = scope_rename_fast(&ast, root);
        let owned = scope_rename_fast_owned(ast, root);
        assert_eq!(owned.root, expected.root);
        assert!(owned.ast.nodes == expected.ast.nodes);
        assert_eq!(
            owned.ast.strings.all_strings(),
            expected.ast.strings.all_strings()
        );
        assert_eq!(
            serde_json::to_vec(&owned.ast).unwrap(),
            serde_json::to_vec(&expected.ast).unwrap()
        );
        assert!(owned.mapping.is_empty());
        // Untracked inputs take the same path with no origins to share.
        let (plain, plain_root) = storm_lua_syntax::parser::parse_source(source).unwrap();
        let expected_plain = scope_rename_fast(&plain, plain_root);
        let owned_plain = scope_rename_fast_owned(plain, plain_root);
        assert_eq!(owned_plain.root, expected_plain.root);
        assert!(owned_plain.ast.nodes == expected_plain.ast.nodes);
        assert_eq!(
            owned_plain.ast.strings.all_strings(),
            expected_plain.ast.strings.all_strings()
        );
    }

    #[test]
    fn tracked_cost_model_matches_the_real_rename_without_mutating_provenance() {
        let source = "local descriptive=1 function onTick()local other=descriptive+1 output.setNumber(1,other)end";
        let (ast, root) = storm_lua_syntax::parse_source_with_origins("cost.lua", source).unwrap();
        let before = serde_json::to_vec(&ast).unwrap();
        let expected = scope_rename_fast(&ast, root);
        assert_eq!(
            measure_renamed_size(&ast, root),
            storm_lua_syntax::size::measure_size(&expected.ast, expected.root)
        );
        assert_eq!(serde_json::to_vec(&ast).unwrap(), before);
        assert!(expected.ast.nodes.tracks_origins());
    }

    #[test]
    fn renamed_size_memo_agrees_after_mutation_and_interning() {
        let source = "local descriptive=1 function onTick()local other=descriptive+1 output.setNumber(1,other)end";
        let (mut ast, root) =
            storm_lua_syntax::parse_source_with_origins("memo.lua", source).unwrap();
        let first = measure_renamed_size(&ast, root);
        // Second call is served by the memo and must agree.
        assert_eq!(measure_renamed_size(&ast, root), first);
        let materialized = scope_rename_fast(&ast, root);
        assert_eq!(
            first,
            storm_lua_syntax::size::measure_size(&materialized.ast, materialized.root)
        );
        // Interning an unrelated identifier must not change the result
        // (the memo revalidates against the symbol count either way).
        ast.strings.intern("unrelated_memo_name");
        assert_eq!(measure_renamed_size(&ast, root), first);
        // A syntax mutation (here: append an unreferenced local to the
        // root block) advances the arena generation; the memoized value
        // must not be served for the changed tree.
        let appended = ast.strings.intern("appended_memo_local");
        let extra = ast.push(storm_lua_syntax::ast::Node::Local(vec![appended], vec![]));
        let block = match &ast.nodes[root as usize] {
            storm_lua_syntax::ast::Node::Block(stmts) => {
                let mut stmts = stmts.clone();
                stmts.push(extra);
                stmts
            }
            _ => panic!("root is not a block"),
        };
        ast.nodes
            .rewrite(root, storm_lua_syntax::ast::Node::Block(block), "memo-test");
        let after = measure_renamed_size(&ast, root);
        let materialized = scope_rename_fast(&ast, root);
        assert_eq!(
            after,
            storm_lua_syntax::size::measure_size(&materialized.ast, materialized.root)
        );
    }

    #[test]
    fn renamed_size_unmemoized_agrees_and_does_not_pin_the_buffers() {
        let source = "local descriptive=1 function onTick()local other=descriptive+1 output.setNumber(1,other)end";
        let (ast, root) = storm_lua_syntax::parse_source_with_origins("memo.lua", source).unwrap();
        let values = ast.nodes.values_arc();
        let strings = ast.strings.strings_arc();
        let values_before = Arc::strong_count(&values);
        let strings_before = Arc::strong_count(&strings);
        let unmemoized = measure_renamed_size_unmemoized(&ast, root);
        // The unmemoized route must not retain the arena's buffers: the
        // strong counts are exactly what they were before the call,
        // unlike a memo insertion, which keeps both buffers alive.
        assert_eq!(Arc::strong_count(&values), values_before);
        assert_eq!(Arc::strong_count(&strings), strings_before);
        // And it must agree with the memoized route.
        assert_eq!(measure_renamed_size(&ast, root), unmemoized);
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

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod cost_only_name_tests {
    use super::*;
    #[test]
    fn borrowed_rename_size_matches_materialized_ast_for_every_registered_fixture() {
        for &id in crate::pass_ids::OPTIMIZATION_PASS_IDS {
            let text = crate::provenance_pass_matrix::fixture(id);
            let (mut ast, root) =
                storm_lua_syntax::parse_source_with_origins("cost.lua", &text).unwrap();
            let root = crate::provenance_pass_matrix::apply(id, &mut ast, root);
            let frozen = serde_json::to_vec(&ast).unwrap();
            let actual = measure_renamed_size(&ast, root);
            let old = scope_rename_fast(&ast, root);
            assert_eq!(
                actual,
                storm_lua_syntax::size::measure_size(&old.ast, old.root),
                "{id}"
            );
            assert_eq!(
                serde_json::to_vec(&ast).unwrap(),
                frozen,
                "{id}: cost calculation mutated source"
            );
        }
    }
    #[test]
    fn cost_only_names_do_not_rename_fields_methods_labels_or_shadowed_declarations() {
        for text in [
            "local first,second=1,2 for first=1,10 do for second,third in pairs({hello=first})do print(second,third)end end",
            "local function recurse(n)if n>0 then return recurse(n-1)end end return recurse(3)",
            "local object={}function object:method(value)return self.field+value end return object:method(7)",
            "local object={}function object.method(a,...)return a,... end return object.method(1,2)",
            "local result='雪😀' ::again::do local result=result output.setNumber(1,#result)end if flag then goto again end",
            "local same=1 do local same=same+1 print(same)end print(same)",
            "local a,a=1,2 local function f(a,a)return a end print(a,f(3,4))",
            "local unused = nil local a,b,c=f() output.setNumber(1,a+b+c)",
        ] {
            let (ast,root)=storm_lua_syntax::parse_source_with_origins("names.lua",text).unwrap();
            let materialized=scope_rename_fast(&ast,root);
            assert_eq!(measure_renamed_size(&ast,root),storm_lua_syntax::size::measure_size(&materialized.ast,materialized.root),"{text}");
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod compact_capture_tests {
    use super::*;
    fn reference_subtree_refs(resolution: &Resolution) -> Vec<HashSet<u32>> {
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

    fn reference_nested_scope(
        sid: ScopeId,
        resolution: &Resolution,
        names: &mut RenameNames<'_>,
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
                    let name = candidates.next().into_owned();
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
            names[*id as usize] = Some(Cow::Owned(q));
            assigned.push((*id, index));
        }
        for &c in &sc.children {
            reference_nested_scope(c, resolution, names, subtree_refs);
        }
    }

    fn compare(ast: &Ast, root: NodeId) {
        let (resolution, actual) = rename_plan(ast, root);
        let expected_refs = reference_subtree_refs(&resolution);
        let actual_refs = get_subtree_refs(&resolution);
        for (expected, actual) in expected_refs.iter().zip(&actual_refs) {
            assert_eq!(actual.iter().copied().collect::<HashSet<_>>(), *expected);
        }
        let mut names = vec![None; resolution.bindings.len()];
        let mut used = HashSet::new();
        rename_globals(ast, &resolution, &mut names, &mut used, false);
        rename_top_locals(ast, &resolution, &mut names, &mut used, false);
        for &scope in &resolution.scope(0).children {
            reference_nested_scope(scope, &resolution, &mut names, &expected_refs);
        }
        assert_eq!(
            actual, names,
            "capture restrictions or short-name tie order changed"
        );
        let (a, r) = apply_renames(ast, &resolution, root, &actual);
        let (b, s) = apply_renames(ast, &resolution, root, &names);
        assert_eq!(r, s);
        assert_eq!(
            serde_json::to_value(&a).unwrap(),
            serde_json::to_value(&b).unwrap()
        );
    }
    #[test]
    fn ordered_capture_membership_matches_legacy_hashes_on_each_pass_fixture_and_rewrite() {
        for id in crate::pass_ids::OPTIMIZATION_PASS_IDS {
            let source = crate::provenance_pass_matrix::fixture(id);
            let (ast, root) =
                storm_lua_syntax::parse_source_with_origins("capture.lua", &source).unwrap();
            compare(&ast, root);
            let mut after = ast.clone();
            let root = crate::provenance_pass_matrix::apply(id, &mut after, root);
            compare(&after, root);
            compare(
                &storm_lua_syntax::ast_utils::clone_without_origins(&after),
                root,
            );
        }
        for n in [1, 16, 64] {
            let source=format!("local x=1 local y=2 function onTick(){}end",(0..n).map(|i|format!("do local x=x+{i} local function f(y)return x+y end output.setNumber(1,f(y))end ")).collect::<String>());
            let (ast, root) =
                storm_lua_syntax::parse_source_with_origins("nested.lua", &source).unwrap();
            compare(&ast, root);
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod borrowed_short_names_tests {
    use super::*;
    fn original_name(len: usize, index: u64) -> String {
        let mut v = index;
        let mut text = String::with_capacity(len);
        for pos in 0..len {
            let place = (SHORT_NAME_TAIL.len() as u64).pow((len - 1 - pos) as u32);
            let alphabet = if pos == 0 {
                SHORT_NAME_FIRST
            } else {
                SHORT_NAME_TAIL
            };
            let digit = (v / place) % alphabet.len() as u64;
            text.push(alphabet[digit as usize] as char);
            v %= place.max(1);
        }
        text
    }
    #[test]
    fn borrowed_names_keep_the_complete_mixed_radix_order_and_owned_public_iterator() {
        let mut generator = ShortNames::new();
        let mut expected = Vec::new();
        for len in 1..=3 {
            let count = (SHORT_NAME_FIRST.len() as u64)
                * (SHORT_NAME_TAIL.len() as u64).pow((len - 1) as u32);
            for index in 0..count.min(10000) {
                let want = original_name(len, index);
                let actual = generator.next_candidate();
                assert_eq!(actual, want);
                assert_eq!(matches!(actual, Cow::Borrowed(_)), len == 1);
                if !reserved(&want) && !api_roots(&want) {
                    expected.push(want);
                }
            }
        }
        let actual: Vec<String> = short_names_debug().take(expected.len()).collect();
        assert_eq!(actual, expected);
    }
    #[test]
    fn short_name_plans_borrow_single_character_assignments_without_changing_source_names() {
        let source =
            "function onTick(first,second)local value=first+second output.setNumber(1,value)end";
        let (ast, root) = storm_lua_syntax::parse_source_with_origins("short.lua", source).unwrap();
        let (res, names) = rename_plan(&ast, root);
        for (i, name) in names.iter().enumerate().skip(1) {
            if let Some(name) = name {
                if res.bindings[i].kind != BindingKind::Global {
                    assert!(matches!(name, Cow::Borrowed(_)));
                }
            }
        }
        let result = scope_rename(&ast, root);
        assert_eq!(
            storm_lua_syntax::size::measure_size(&result.ast, result.root),
            measure_renamed_size(&ast, root)
        );
        let printed =
            storm_lua_syntax::Printer::new(&result.ast, false).output_with_positions(result.root);
        let origins =
            storm_lua_syntax::provenance::GeneratedOrigins::from_print(&result.ast, &printed)
                .unwrap();
        origins.validate_for_code(&printed.code).unwrap();
        assert_eq!(origins.unknown_bytes(), 0);
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod borrowed_global_names_tests {
    use super::*;
    fn reference_globals(
        ast: &Ast,
        resolution: &Resolution,
        names: &mut RenameNames<'_>,
        used: &mut HashSet<String>,
    ) {
        for (i, b) in resolution.bindings.iter().enumerate() {
            if i == 0 {
                continue;
            }
            if b.fixed || (b.kind == BindingKind::Global && resolution.binding_write_counts[i] == 0)
            {
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
                names[id as usize] = Some(Cow::Owned(ast.strings.get(b.name).to_string()));
                continue;
            }
            let q = loop {
                let q = git.next();
                if !used.contains(q.as_ref()) {
                    break q;
                }
            };
            names[id as usize] = Some(q.clone());
            used.insert(q.into_owned());
        }
    }

    fn reference_top_locals(
        _ast: &Ast,
        resolution: &Resolution,
        names: &mut RenameNames<'_>,
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
                if !used.contains(q.as_ref()) && !own.contains(q.as_ref()) {
                    break q;
                }
            };
            names[id as usize] = Some(q.clone());
            own.insert(q.into_owned());
        }
    }

    fn compare(ast: &Ast, root: NodeId) {
        let resolution = resolve(ast, root);
        let mut expected = vec![None; resolution.bindings.len()];
        let mut used = HashSet::new();
        reference_globals(ast, &resolution, &mut expected, &mut used);
        reference_top_locals(ast, &resolution, &mut expected, &mut used);
        let captures = get_subtree_refs(&resolution);
        for &child in &resolution.scope(0).children {
            rename_nested_scope(ast, child, &resolution, &mut expected, &captures, false);
        }
        let (_, actual) = rename_plan(ast, root);
        assert_eq!(actual, expected);
        let (a, ar) = apply_renames(ast, &resolution, root, &actual);
        let (b, br) = apply_renames(ast, &resolution, root, &expected);
        assert_eq!(ar, br);
        assert_eq!(
            serde_json::to_value(a).unwrap(),
            serde_json::to_value(b).unwrap()
        );
    }
    #[test]
    fn borrowed_fixed_names_and_unique_local_sequence_match_the_owned_reference() {
        for pass in crate::pass_ids::OPTIMIZATION_PASS_IDS {
            let text = crate::provenance_pass_matrix::fixture(pass);
            let (mut ast, root) =
                storm_lua_syntax::parse_source_with_origins("rename.lua", &text).unwrap();
            compare(&ast, root);
            let root = crate::provenance_pass_matrix::apply(pass, &mut ast, root);
            compare(&ast, root);
        }
        for count in [1, 10, 54, 128, 512] {
            let mut source = (0..count)
                .map(|i| format!("local variable{i}={i} "))
                .collect::<String>();
            source.push_str("function onTick() ");
            for i in 0..count {
                source.push_str(&format!("output.setNumber(1,variable{i}+unassigned{i}) "));
            }
            source.push_str("end");
            let (ast, root) =
                storm_lua_syntax::parse_source_with_origins("many.lua", &source).unwrap();
            compare(&ast, root);
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod mapping_aware_print_tests {
    use super::*;

    /// Mapping-aware print must equal stock print of the renamed tree,
    /// including the merge-group cases where renamed and unrenamed trees
    /// decide differently (shadowed same-spelling members, references to
    /// group members, captured references inside initializers).
    #[test]
    fn mapping_aware_print_matches_renamed_print() {
        let sources = [
            // Shadowed members: stock blocks the merge on the unrenamed
            // tree (same spelling) but merges on the renamed tree.
            "local long_name = 1 local long_name = 2 output.setNumber(1, long_name)",
            // Member initializer mentioning an earlier member: blocks in both.
            "local first_value = 10 local second_value = first_value + 1 output.setNumber(1, second_value)",
            // Member initializer mentioning an outer binding of the same
            // spelling as a member: stock blocks on the unrenamed tree.
            "local shared = 5 do local shared = 1 local other = shared output.setNumber(1, other) end",
            // Captured reference inside a nested function initializer.
            "local base = 3 local make = function() return base end output.setNumber(1, make())",
            // Method self is never renamed (candidate AA): both paths agree.
            "local obj = { value = 7 } function obj:get() return self.value end output.setNumber(1, obj:get())",
            // Several locals, one group, trailing values.
            "local aa = 1 local bb = 2 local cc = aa + bb output.setNumber(1, cc)",
            // Globals keep their spelling; only locals shorten.
            "output.setNumber(1, some_global) local local_name = 4 output.setNumber(2, local_name)",
        ];
        for src in sources {
            let (ast, root) =
                storm_lua_syntax::parse_source_with_origins("mapprint.lua", src).unwrap();
            let renamed = scope_rename_fast(&ast, root);
            let stock = storm_lua_syntax::Printer::new(&renamed.ast, false).output(renamed.root);
            let mapped = print_with_rename_mapping(&ast, root, false);
            assert_eq!(mapped, stock, "source: {src}");
            // Zero-cost-newline mode must agree as well.
            let stock_zero =
                storm_lua_syntax::Printer::new(&renamed.ast, true).output(renamed.root);
            let mapped_zero = print_with_rename_mapping(&ast, root, true);
            assert_eq!(mapped_zero, stock_zero, "source (zero): {src}");
        }
    }
}
