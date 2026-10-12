//! Canonical effect analysis implementation.
//!
//! Effects / mergeEffects / effectKey と EffectAnalyzer（apiAliases fixpoint 8 周、
//! summaries fixpoint 20 周）を TS と同一手順で実装する。決定性のため reads/writes は
//! ソート済み配列で保持し、ダンプ時はビッド昇順（＝挿入順）で扱う。

use crate::resolver::{api_roots, for_each_child, BindingId, BindingKind, Resolution};
use storm_lua_syntax::ast::{Ast, Node, NodeId};

#[derive(Clone, Debug, PartialEq)]
pub struct Effects {
    pub reads: Vec<BindingId>,
    pub writes: Vec<BindingId>,
    pub calls: bool,
    pub ordered: bool,
    pub may_throw: bool,
    pub stable: bool,
}

pub fn empty_effects() -> Effects {
    Effects {
        reads: Vec::new(),
        writes: Vec::new(),
        calls: false,
        ordered: false,
        may_throw: false,
        stable: true,
    }
}

/// マージ後も決定性を保つため、reads/writes は昇順ソート済みのユニーク配列で返す。
pub fn merge_effects(xs: &[Effects]) -> Effects {
    let read_capacity = xs.iter().map(|x| x.reads.len()).sum();
    let write_capacity = xs.iter().map(|x| x.writes.len()).sum();
    let mut z = Effects {
        reads: Vec::with_capacity(read_capacity),
        writes: Vec::with_capacity(write_capacity),
        calls: false,
        ordered: false,
        may_throw: false,
        stable: true,
    };
    for x in xs {
        z.reads.extend_from_slice(&x.reads);
        z.writes.extend_from_slice(&x.writes);
        z.calls |= x.calls;
        z.ordered |= x.ordered;
        z.may_throw |= x.may_throw;
        z.stable &= x.stable;
    }
    z.reads.sort_unstable();
    z.reads.dedup();
    z.writes.sort_unstable();
    z.writes.dedup();
    z
}

/// Internal accumulation may contain duplicate bindings. Normalize only at an
/// analysis boundary, rather than allocating and sorting at every AST edge.
fn normalize_effects(mut e: Effects) -> Effects {
    e.reads.sort_unstable();
    e.reads.dedup();
    e.writes.sort_unstable();
    e.writes.dedup();
    e
}

pub fn effect_key(e: &Effects) -> String {
    let reads: Vec<String> = e.reads.iter().map(u32::to_string).collect();
    let writes: Vec<String> = e.writes.iter().map(u32::to_string).collect();
    format!(
        "{}|{}|{}{}{}{}",
        reads.join(","),
        writes.join(","),
        e.calls as u8,
        e.ordered as u8,
        e.may_throw as u8,
        e.stable as u8
    )
}

/// TS `walk()` の Preorder 順（キー挿入順の再帰トラバーサル。bid/bids/scopeId/write は
/// ノードを含まないため forEachChild と同一の子順序になる）。
pub fn walk(ast: &Ast, id: NodeId, out: &mut Vec<NodeId>) {
    out.push(id);
    for_each_child(ast, id, &mut |c| walk(ast, c, out));
}

// Public compatibility lists and the private lookup share one declaration.
// Classification uses the proven builtin's two borrowed name components rather
// than testing each full vocabulary entry with repeated prefix comparisons.
macro_rules! builtin_set {
    ($list:ident,$lookup:ident;[$(($root:literal,$member:literal)),* $(,)?];[$($global:literal),* $(,)?])=>{
        pub const $list:&[&str]=&[$(concat!($root,".",$member),)*$($global),*];
        fn $lookup(reference:&BuiltinReference<'_>)->bool {
            match reference.parts() {
                $(($root,Some($member))=>true,)*
                $(($global,None)=>true,)*
                _=>false,
            }
        }
    };
}
builtin_set!(PURE_API,is_pure_api;[("math","pi"),("math","sin"),("math","cos"),("math","tan"),("math","atan"),("math","sqrt"),("math","abs"),("math","max"),("math","min"),("math","floor"),("input","getNumber"),("input","getBool"),("property","getNumber"),("property","getBool"),("string","pack"),("string","unpack"),("string","format"),("table","unpack")];["type","tonumber","tostring"]);
builtin_set!(TOTAL_API,is_total_api;[("input","getNumber"),("input","getBool"),("property","getNumber"),("property","getBool"),("math","abs"),("math","max"),("math","min"),("math","floor"),("math","sin"),("math","cos"),("math","tan"),("math","atan")];["type","tostring"]);
builtin_set!(EFFECT_API,is_effect_api;[("output","setNumber"),("output","setBool")];[]);

#[derive(Clone, Debug, PartialEq)]
pub struct FunctionSummary {
    pub pure: bool,
    pub total: bool,
    pub reads: Vec<BindingId>,
    pub writes: Vec<BindingId>,
    pub calls: bool,
    pub ordered: bool,
    pub may_throw: bool,
    pub stable: bool,
}

impl FunctionSummary {
    fn from_effects(e: Effects) -> Self {
        Self {
            pure: !e.calls && !e.ordered && e.writes.is_empty(),
            total: !e.may_throw,
            reads: e.reads,
            writes: e.writes,
            calls: e.calls,
            ordered: e.ordered,
            may_throw: e.may_throw,
            stable: e.stable,
        }
    }
    fn same_effect_key(&self, e: &Effects) -> bool {
        self.reads == e.reads
            && self.writes == e.writes
            && self.calls == e.calls
            && self.ordered == e.ordered
            && self.may_throw == e.may_throw
            && self.stable == e.stable
    }
}

// A proven builtin reference keeps its root/alias and last member separate.
// Calls usually use a single quoted member, so effect classification need not
// allocate either a decoded key or a concatenated qualified name.
struct BuiltinReference<'a> {
    base: std::borrow::Cow<'a, str>,
    member: Option<std::borrow::Cow<'a, str>>,
}
impl<'a> BuiltinReference<'a> {
    fn parts(&self) -> (&str, Option<&str>) {
        match self.member.as_deref() {
            Some(member) => (self.base.as_ref(), Some(member)),
            None => self
                .base
                .split_once('.')
                .map_or((self.base.as_ref(), None), |(root, member)| {
                    (root, Some(member))
                }),
        }
    }

    fn is(&self, name: &str) -> bool {
        match &self.member {
            None => self.base == name,
            Some(member) => name
                .strip_prefix(self.base.as_ref())
                .and_then(|suffix| suffix.strip_prefix('.'))
                .is_some_and(|suffix| suffix == member.as_ref()),
        }
    }
    fn starts_with(&self, prefix: &str) -> bool {
        self.base.starts_with(prefix)
            || self.member.as_ref().is_some_and(|member| {
                prefix
                    .strip_prefix(self.base.as_ref())
                    .and_then(|tail| tail.strip_prefix('.'))
                    .is_some_and(|tail| member.starts_with(tail))
            })
    }
    fn into_text(self) -> std::borrow::Cow<'a, str> {
        match self.member {
            None => self.base,
            Some(member) => std::borrow::Cow::Owned(format!("{}.{}", self.base, member)),
        }
    }
}
fn borrowed_member(raw: &str) -> std::borrow::Cow<'_, str> {
    let bytes = raw.as_bytes();
    if bytes.len() >= 2 && matches!(bytes[0], b'\'' | b'"') && bytes.last() == Some(&bytes[0]) {
        let inner = &raw[1..raw.len() - 1];
        if !inner.as_bytes().contains(&b'\\') {
            return std::borrow::Cow::Borrowed(inner);
        }
    }
    std::borrow::Cow::Owned(decode_lua_string(raw))
}

pub struct EffectAnalyzer<'a> {
    ast: &'a Ast,
    res: &'a Resolution,
    pub stable_reads: bool,
    /// bid → 実ノード。bindings の function_node から構築（bid 昇順）。
    pub functions: Vec<Option<NodeId>>,
    /// bid → summary（bid 昇順）。未確定関数は None 以外の初期値を持つ。
    pub summaries: Vec<Option<FunctionSummary>>,
    /// bid → エイリアス名（挿入順の決定性のため Vec で保持。dump はこの順で出力）。
    pub api_alias_order: Vec<(BindingId, String)>,
    api_alias_map: Vec<Option<String>>,
}

impl<'a> EffectAnalyzer<'a> {
    pub fn new(ast: &'a Ast, res: &'a Resolution, root: NodeId, stable_reads: bool) -> Self {
        let mut writes = vec![0u32; res.bindings.len()];
        let mut analyzer = Self {
            ast,
            res,
            stable_reads,
            functions: vec![None; res.bindings.len()],
            summaries: vec![None; res.bindings.len()],
            api_alias_order: Vec::new(),
            api_alias_map: vec![None; res.bindings.len()],
        };

        // Count actual root-reachable occurrences and collect builtin roots in
        // the same traversal. Do not materialize a complete preorder node list.
        // A later write may invalidate a root first seen as a read; remove those
        // candidates before any alias propagation or effect query can use them.
        analyzer.seed_root_occurrences(root, &mut writes);
        analyzer.api_alias_order.retain(|(bid, _)| {
            if writes[*bid as usize] == 0 {
                true
            } else {
                analyzer.api_alias_map[*bid as usize] = None;
                false
            }
        });

        // apiAliases fixpoint（8 周）
        if let Node::Block(ss) = &ast.nodes[root as usize] {
            for _ in 0..8 {
                let mut changed = false;
                for &s in ss {
                    let Node::Assign(vs, es) = &ast.nodes[s as usize] else {
                        continue;
                    };
                    for (index, &target) in vs.iter().enumerate() {
                        let expression = es.get(index);
                        let Some(expression) = expression else {
                            continue;
                        };
                        let Node::Name(_) = ast.nodes[target as usize] else {
                            continue;
                        };
                        let Some(tbid) = res.node_bid[target as usize] else {
                            continue;
                        };
                        if writes[tbid as usize] != 1 {
                            continue;
                        }
                        // Borrow already-proven names while testing this fixed
                        // point. An unchanged target needs no cloned/assembled
                        // qualified name; allocate only for an actual update.
                        let alias = match &ast.nodes[*expression as usize] {
                            Node::Name(_) => res.node_bid[*expression as usize]
                                .and_then(|bid| analyzer.api_alias_get(bid))
                                .map(|name| BuiltinReference {
                                    base: std::borrow::Cow::Borrowed(name),
                                    member: None,
                                }),
                            Node::Index(obj, key, _) => {
                                if let (Node::Name(_), Node::Str(raw)) =
                                    (ast.node(*obj), ast.node(*key))
                                {
                                    res.node_bid[*obj as usize]
                                        .and_then(|bid| analyzer.api_alias_get(bid))
                                        .map(|base| BuiltinReference {
                                            base: std::borrow::Cow::Borrowed(base),
                                            member: Some(borrowed_member(raw)),
                                        })
                                } else {
                                    None
                                }
                            }
                            _ => None,
                        };
                        if let Some(alias) = alias {
                            if analyzer
                                .api_alias_get(tbid)
                                .is_none_or(|current| !alias.is(current))
                            {
                                let owned = alias.into_text().into_owned();
                                analyzer.api_alias_set(tbid, owned);
                                changed = true;
                            }
                        }
                    }
                }
                if !changed {
                    break;
                }
            }
        }

        // functions / summaries（bid 昇順。TS は bindings Map の挿入順＝bid 順）
        let mut function_bindings = Vec::new();
        for (bid, binding) in res.bindings.iter().enumerate().skip(1) {
            if let Some(fn_) = binding.function_node {
                analyzer.functions[bid] = Some(fn_);
                function_bindings.push((bid as BindingId, fn_));
            }
        }
        if function_bindings.is_empty() {
            return analyzer;
        }
        for &(bid, _) in &function_bindings {
            {
                analyzer.summaries[bid as usize] = Some(FunctionSummary {
                    pure: false,
                    total: false,
                    reads: Vec::new(),
                    writes: Vec::new(),
                    calls: false,
                    ordered: false,
                    may_throw: false,
                    stable: true,
                });
            }
        }
        // A summary depends only on this immutable body, the fixed alias table
        // and summaries read through direct calls. Every such call records its
        // callee binding in Effects.reads, even before its summary has stabilized.
        // Keep a conservative read-dependency set from the first evaluation;
        // repeated rounds may skip a body only while those versions are unchanged.
        let mut versions = vec![0u32; res.bindings.len()];
        let mut dependencies: Vec<Option<Vec<(BindingId, u32)>>> =
            (0..function_bindings.len()).map(|_| None).collect();
        for _ in 0..20 {
            let mut changed = false;
            // The worklist retains binding order, round count and immediate
            // summary updates. Non-function bindings cannot enter this fixpoint.
            for (function_index, &(bid, fn_)) in function_bindings.iter().enumerate() {
                if dependencies[function_index].as_ref().is_some_and(|deps| {
                    deps.iter()
                        .all(|&(callee, version)| versions[callee as usize] == version)
                }) {
                    continue;
                }
                // functions は「関数ノード」を保持（TS の b.functionNode）。本体 block を開く。
                let body = match &ast.nodes[fn_ as usize] {
                    Node::Function(_, _, b) => *b,
                    _ => unreachable!(),
                };
                let e = analyzer.block_effects(body, Some(bid));
                match &mut dependencies[function_index] {
                    Some(deps) => {
                        for (callee, version) in deps {
                            *version = versions[*callee as usize];
                        }
                    }
                    slot @ None => {
                        *slot = Some(
                            e.reads
                                .iter()
                                .copied()
                                .filter(|&callee| analyzer.functions[callee as usize].is_some())
                                .map(|callee| (callee, versions[callee as usize]))
                                .collect(),
                        );
                    }
                }
                // TS は effectKey のみで変化判定する（pure/total は effectKey の比較対象外）。
                // 初期値と同一の効果なら更新されず、pure=false total=false が残る。
                // summaries は fixpoint 開始前に全関数 bid へ Some を注入済みのため None はない。
                #[expect(
                    clippy::expect_used,
                    reason = "Every function-body binding is initialized before the fixed-point loop; no user callback runs during analysis"
                )]
                let old = analyzer.summaries[bid as usize]
                    .as_ref()
                    .expect("summaries は初期化済み");
                if !old.same_effect_key(&e) {
                    analyzer.summaries[bid as usize] = Some(FunctionSummary::from_effects(e));
                    versions[bid as usize] += 1;
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }

        analyzer
    }

    fn api_alias_get(&self, bid: BindingId) -> Option<&String> {
        self.api_alias_map[bid as usize].as_ref()
    }
    fn seed_root_occurrences(&mut self, node: NodeId, writes: &mut [u32]) {
        if let Node::Name(symbol) = self.ast.nodes[node as usize] {
            if let Some(bid) = self.res.node_bid[node as usize] {
                if self.res.node_write[node as usize] {
                    writes[bid as usize] += 1;
                }
                let name = self.ast.strings.get(symbol);
                let binding = &self.res.bindings[bid as usize];
                if writes[bid as usize] == 0
                    && api_roots(name)
                    && binding.kind == BindingKind::Global
                    && self.ast.strings.get(binding.name) == name
                    && self.api_alias_get(bid).is_none()
                {
                    self.api_alias_set(bid, name.to_owned());
                }
            }
        }
        let ast = self.ast;
        for_each_child(ast, node, &mut |child| {
            self.seed_root_occurrences(child, writes)
        });
    }

    fn api_alias_set(&mut self, bid: BindingId, alias: String) {
        if self.api_alias_map[bid as usize].is_none() {
            self.api_alias_order.push((bid, alias.clone()));
        }
        self.api_alias_map[bid as usize] = Some(alias);
    }

    /// 証明済み single-write エイリアスのみを通してビルトイン root/member を解決する。
    pub fn resolve_builtin_reference(&self, x: NodeId) -> Option<String> {
        self.builtin_reference(x)
            .map(|value| value.into_text().into_owned())
    }
    fn builtin_reference(&self, x: NodeId) -> Option<BuiltinReference<'_>> {
        match &self.ast.nodes[x as usize] {
            Node::Name(_) => Some(BuiltinReference {
                base: std::borrow::Cow::Borrowed(
                    self.api_alias_get(self.res.node_bid[x as usize]?)?,
                ),
                member: None,
            }),
            Node::Index(obj, key, _) => {
                let Node::Str(raw) = &self.ast.nodes[*key as usize] else {
                    return None;
                };
                let base = self.builtin_reference(*obj)?.into_text();
                Some(BuiltinReference {
                    base,
                    member: Some(borrowed_member(raw)),
                })
            }
            _ => None,
        }
    }

    // A single accumulator avoids a Vec<Effects> and two binding-vector
    // allocations per expression. No results are cached across mutable summary
    // fixpoint iterations or changes to the public stable_reads setting.
    fn expr_into<const READS: bool>(&self, x: NodeId, e: &mut Effects) {
        match &self.ast.nodes[x as usize] {
            Node::Name(_) => {
                if let Some(bid) = self.res.node_bid[x as usize] {
                    if READS {
                        e.reads.push(bid);
                    }
                }
            }
            Node::Num(_)
            | Node::Str(_)
            | Node::Nil
            | Node::Bool(_)
            | Node::Vararg
            | Node::Function(..) => {}
            Node::Paren(x) => self.expr_into::<READS>(*x, e),
            Node::Table(fs) => {
                for f in fs {
                    match f {
                        storm_lua_syntax::ast::TableField::Arr(v)
                        | storm_lua_syntax::ast::TableField::Name(_, v) => {
                            self.expr_into::<READS>(*v, e);
                        }
                        storm_lua_syntax::ast::TableField::KVar(k, v) => {
                            self.expr_into::<READS>(*k, e);
                            self.expr_into::<READS>(*v, e);
                        }
                    }
                }
            }
            Node::Index(obj, key, _) => {
                self.expr_into::<READS>(*obj, e);
                self.expr_into::<READS>(*key, e);
                e.may_throw = true;
            }
            Node::Un(op, x) => {
                self.expr_into::<READS>(*x, e);
                e.may_throw |= matches!(op.as_str(), "-" | "#" | "~");
            }
            Node::Bin(op, l, r) => {
                self.expr_into::<READS>(*l, e);
                self.expr_into::<READS>(*r, e);
                e.may_throw |= !matches!(op.as_str(), "and" | "or" | "==" | "~=");
            }
            Node::Call(function, args, _) => {
                let full = self.builtin_reference(*function);
                // A proven builtin's callee lookup is total. Only suppress the
                // callee's throw bit, never a previous sibling's or argument's.
                let preceding_throw = e.may_throw;
                self.expr_into::<READS>(*function, e);
                if full.is_some() {
                    e.may_throw = preceding_throw;
                }
                for &arg in args {
                    self.expr_into::<READS>(arg, e);
                }
                if let Some(full) = &full {
                    if full.starts_with("screen.")
                        && !full.is("screen.getWidth")
                        && !full.is("screen.getHeight")
                    {
                        e.calls = true;
                        e.ordered = true;
                        e.stable = false;
                        return;
                    }
                    if is_pure_api(full) {
                        e.may_throw |= !is_total_api(full);
                        e.stable &= self.stable_reads
                            || (!full.starts_with("input.") && !full.starts_with("property."));
                        return;
                    }
                    if is_effect_api(full) {
                        e.calls = true;
                        e.stable = false;
                        return;
                    }
                }
                if let (Node::Name(_), Some(bid)) = (
                    &self.ast.nodes[*function as usize],
                    self.res.node_bid[*function as usize],
                ) {
                    if let Some(s) = &self.summaries[bid as usize] {
                        if READS {
                            e.reads.extend_from_slice(&s.reads);
                        }
                        e.writes.extend_from_slice(&s.writes);
                        e.calls |= s.calls;
                        e.ordered |= s.ordered;
                        e.may_throw |= s.may_throw;
                        e.stable &= s.stable;
                        return;
                    }
                }
                e.calls = true;
                e.may_throw = true;
                e.stable = false;
            }
            _ => for_each_child(self.ast, x, &mut |q| self.expr_into::<READS>(q, e)),
        }
    }

    #[cfg(test)]
    fn allocating_expr_into<const READS: bool>(&self, x: NodeId, e: &mut Effects) {
        match &self.ast.nodes[x as usize] {
            Node::Name(_) => {
                if let Some(bid) = self.res.node_bid[x as usize] {
                    if READS {
                        e.reads.push(bid);
                    }
                }
            }
            Node::Num(_)
            | Node::Str(_)
            | Node::Nil
            | Node::Bool(_)
            | Node::Vararg
            | Node::Function(..) => {}
            Node::Paren(x) => self.allocating_expr_into::<READS>(*x, e),
            Node::Table(fs) => {
                for f in fs {
                    match f {
                        storm_lua_syntax::ast::TableField::Arr(v)
                        | storm_lua_syntax::ast::TableField::Name(_, v) => {
                            self.allocating_expr_into::<READS>(*v, e);
                        }
                        storm_lua_syntax::ast::TableField::KVar(k, v) => {
                            self.allocating_expr_into::<READS>(*k, e);
                            self.allocating_expr_into::<READS>(*v, e);
                        }
                    }
                }
            }
            Node::Index(obj, key, _) => {
                self.allocating_expr_into::<READS>(*obj, e);
                self.allocating_expr_into::<READS>(*key, e);
                e.may_throw = true;
            }
            Node::Un(op, x) => {
                self.allocating_expr_into::<READS>(*x, e);
                e.may_throw |= matches!(op.as_str(), "-" | "#" | "~");
            }
            Node::Bin(op, l, r) => {
                self.allocating_expr_into::<READS>(*l, e);
                self.allocating_expr_into::<READS>(*r, e);
                e.may_throw |= !matches!(op.as_str(), "and" | "or" | "==" | "~=");
            }
            Node::Call(function, args, _) => {
                let full = self.resolve_builtin_reference(*function);
                // A proven builtin's callee lookup is total. Only suppress the
                // callee's throw bit, never a previous sibling's or argument's.
                let preceding_throw = e.may_throw;
                self.allocating_expr_into::<READS>(*function, e);
                if full.is_some() {
                    e.may_throw = preceding_throw;
                }
                for &arg in args {
                    self.allocating_expr_into::<READS>(arg, e);
                }
                if let Some(full) = full.as_deref() {
                    if full.starts_with("screen.")
                        && full != "screen.getWidth"
                        && full != "screen.getHeight"
                    {
                        e.calls = true;
                        e.ordered = true;
                        e.stable = false;
                        return;
                    }
                    if PURE_API.contains(&full) {
                        e.may_throw |= !TOTAL_API.contains(&full);
                        e.stable &= self.stable_reads
                            || (!full.starts_with("input.") && !full.starts_with("property."));
                        return;
                    }
                    if EFFECT_API.contains(&full) {
                        e.calls = true;
                        e.stable = false;
                        return;
                    }
                }
                if let (Node::Name(_), Some(bid)) = (
                    &self.ast.nodes[*function as usize],
                    self.res.node_bid[*function as usize],
                ) {
                    if let Some(s) = &self.summaries[bid as usize] {
                        if READS {
                            e.reads.extend_from_slice(&s.reads);
                        }
                        e.writes.extend_from_slice(&s.writes);
                        e.calls |= s.calls;
                        e.ordered |= s.ordered;
                        e.may_throw |= s.may_throw;
                        e.stable &= s.stable;
                        return;
                    }
                }
                e.calls = true;
                e.may_throw = true;
                e.stable = false;
            }
            _ => for_each_child(self.ast, x, &mut |q| {
                self.allocating_expr_into::<READS>(q, e)
            }),
        }
    }

    fn statement_into<const READS: bool>(&self, statement: NodeId, e: &mut Effects) {
        match &self.ast.nodes[statement as usize] {
            Node::Local(_, es) | Node::Return(es) => {
                for &x in es {
                    self.expr_into::<READS>(x, e);
                }
            }
            Node::Assign(vs, es) => {
                for &x in es {
                    self.expr_into::<READS>(x, e);
                }
                for &v in vs {
                    if let Node::Name(_) = self.ast.nodes[v as usize] {
                        if let Some(bid) = self.res.node_bid[v as usize] {
                            e.writes.push(bid);
                        }
                    } else {
                        // Preserve the existing lvalue analysis contract: only
                        // reads/calls are merged and an indexed store may throw.
                        let mut target = empty_effects();
                        self.expr_into::<READS>(v, &mut target);
                        if READS {
                            e.reads.extend(target.reads);
                        }
                        e.calls |= target.calls;
                        e.may_throw = true;
                    }
                }
            }
            Node::Callstat(x) => self.expr_into::<READS>(*x, e),
            Node::If(arms, otherwise) => {
                for arm in arms {
                    self.expr_into::<READS>(arm.cond, e);
                    self.block_into::<READS>(arm.body, e);
                }
                if let Some(body) = otherwise {
                    self.block_into::<READS>(*body, e);
                }
            }
            Node::While(condition, body) => {
                self.expr_into::<READS>(*condition, e);
                self.block_into::<READS>(*body, e);
            }
            Node::Repeat(body, condition) => {
                self.block_into::<READS>(*body, e);
                self.expr_into::<READS>(*condition, e);
            }
            Node::Do(body) => self.block_into::<READS>(*body, e),
            Node::Fornum(_, a, b, c, body) => {
                self.expr_into::<READS>(*a, e);
                self.expr_into::<READS>(*b, e);
                if let Some(c) = c {
                    self.expr_into::<READS>(*c, e);
                }
                self.block_into::<READS>(*body, e);
            }
            Node::Forin(_, es, body) => {
                for &x in es {
                    self.expr_into::<READS>(x, e);
                }
                self.block_into::<READS>(*body, e);
            }
            _ => {}
        }
    }

    fn block_into<const READS: bool>(&self, block: NodeId, e: &mut Effects) {
        let Node::Block(statements) = &self.ast.nodes[block as usize] else {
            unreachable!()
        };
        for &statement in statements {
            self.statement_into::<READS>(statement, e);
        }
    }

    fn expr_effects(&self, x: NodeId, _current_fn: Option<BindingId>) -> Effects {
        let mut e = empty_effects();
        self.expr_into::<true>(x, &mut e);
        normalize_effects(e)
    }

    fn statement_effects(&self, statement: NodeId, _current_fn: Option<BindingId>) -> Effects {
        let mut e = empty_effects();
        self.statement_into::<true>(statement, &mut e);
        normalize_effects(e)
    }

    fn block_effects(&self, block: NodeId, _current_fn: Option<BindingId>) -> Effects {
        let mut e = empty_effects();
        self.block_into::<true>(block, &mut e);
        normalize_effects(e)
    }

    /// Public read-only view of expression effects for optimization passes.
    pub fn effects_for_expr(&self, x: NodeId) -> Effects {
        self.expr_effects(x, None)
    }

    /// Public read-only view of statement effects for optimization passes.
    pub fn effects_for_statement(&self, statement: NodeId) -> Effects {
        self.statement_effects(statement, None)
    }

    pub fn is_pure(&self, x: NodeId) -> bool {
        // Queries use the same effect rules without collecting/sorting binding
        // reads that their boolean answer cannot observe. Writes are retained.
        let mut e = empty_effects();
        self.expr_into::<false>(x, &mut e);
        !e.calls && !e.ordered && e.writes.is_empty()
    }

    pub fn is_movable(&self, x: NodeId) -> bool {
        let mut e = empty_effects();
        self.expr_into::<false>(x, &mut e);
        !e.calls && !e.ordered && e.writes.is_empty() && !e.may_throw && e.stable
    }
}

pub fn decode_lua_string(s: &str) -> String {
    storm_lua_syntax::numeric::decode_lua_string(s)
}

/// `semanticExpressionKey`: scopeId/bids/write を除外した直列化キー。
/// Rust はこれらを Resolution 配列で持つため、ノードのカノニカルダンプをキーとする。
/// `astSame` は同一 AST 内の 2 ノードの等価判定に使う（mutual AST のみ、cross-ast 不可）。
pub fn semantic_key(ast: &Ast, id: NodeId) -> String {
    storm_lua_syntax::dump::dump(ast, id)
}

/// `astSame`: 2 ノード（同一 Ast 内）の意味等価性。
pub fn ast_same(ast: &Ast, a: NodeId, b: NodeId) -> bool {
    storm_lua_syntax::dump::same(ast, a, b)
}

/// `countBindingUses`: サブツリー内の read/write 使用回数を bid ごとに数える。
pub fn count_binding_uses(
    ast: &Ast,
    res: &Resolution,
    id: NodeId,
) -> (
    std::collections::HashMap<u32, u32>,
    std::collections::HashMap<u32, u32>,
) {
    let mut reads: std::collections::HashMap<u32, u32> = std::collections::HashMap::new();
    let mut writes: std::collections::HashMap<u32, u32> = std::collections::HashMap::new();
    fn collect(
        ast: &Ast,
        res: &Resolution,
        node: NodeId,
        reads: &mut std::collections::HashMap<u32, u32>,
        writes: &mut std::collections::HashMap<u32, u32>,
    ) {
        if let Node::Name(_) = ast.node(node) {
            if let Some(bid) = res.node_bid[node as usize] {
                let target = if res.node_write[node as usize] {
                    &mut *writes
                } else {
                    &mut *reads
                };
                *target.entry(bid).or_insert(0) += 1;
            }
        }
        for_each_child(ast, node, &mut |child| {
            collect(ast, res, child, reads, writes)
        });
    }
    collect(ast, res, id, &mut reads, &mut writes);
    (reads, writes)
}

/// `containsBinding`: サブツリー内に bid への参照が存在するか。
pub fn contains_binding(ast: &Ast, res: &Resolution, id: NodeId, bid: u32) -> bool {
    if matches!(ast.node(id), Node::Name(_)) && res.node_bid[id as usize] == Some(bid) {
        return true;
    }
    let mut found = false;
    for_each_child(ast, id, &mut |child| {
        if !found {
            found = contains_binding(ast, res, child, bid);
        }
    });
    found
}

#[cfg(test)]
impl<'a> EffectAnalyzer<'a> {
    fn reference_new(ast: &'a Ast, res: &'a Resolution, root: NodeId, stable_reads: bool) -> Self {
        // writes カウント: name.write の件数
        let mut writes = vec![0u32; res.bindings.len()];
        let mut nodes: Vec<NodeId> = Vec::new();
        walk(ast, root, &mut nodes);
        for &nid in &nodes {
            if let Node::Name(_) = ast.nodes[nid as usize] {
                if let Some(bid) = res.node_bid[nid as usize] {
                    if res.node_write[nid as usize] {
                        writes[bid as usize] += 1;
                    }
                }
            }
        }

        let mut analyzer = Self {
            ast,
            res,
            stable_reads,
            functions: vec![None; res.bindings.len()],
            summaries: vec![None; res.bindings.len()],
            api_alias_order: Vec::new(),
            api_alias_map: vec![None; res.bindings.len()],
        };

        // 未解決・未 write の global API root だけがビルトイン。
        for &nid in &nodes {
            if let Node::Name(sid) = ast.nodes[nid as usize] {
                let name = ast.strings.get(sid);
                let Some(bid) = res.node_bid[nid as usize] else {
                    continue;
                };
                if !api_roots(name) || writes[bid as usize] != 0 {
                    continue;
                }
                let b = &res.bindings[bid as usize];
                if b.kind == BindingKind::Global && ast.strings.get(b.name) == name {
                    analyzer.api_alias_set(bid, name.to_string());
                }
            }
        }

        // apiAliases fixpoint（8 周）
        if let Node::Block(ss) = &ast.nodes[root as usize] {
            for _ in 0..8 {
                let mut changed = false;
                for &s in ss {
                    let Node::Assign(vs, es) = &ast.nodes[s as usize] else {
                        continue;
                    };
                    for (index, &target) in vs.iter().enumerate() {
                        let expression = es.get(index);
                        let Some(expression) = expression else {
                            continue;
                        };
                        let Node::Name(_) = ast.nodes[target as usize] else {
                            continue;
                        };
                        let Some(tbid) = res.node_bid[target as usize] else {
                            continue;
                        };
                        if writes[tbid as usize] != 1 {
                            continue;
                        }
                        let alias: Option<String> = match &ast.nodes[*expression as usize] {
                            Node::Name(_) => {
                                if let Some(bid) = res.node_bid[*expression as usize] {
                                    analyzer.api_alias_get(bid).cloned()
                                } else {
                                    None
                                }
                            }
                            Node::Index(obj, key, _) => {
                                if let (Node::Name(_), Node::Str(ksid)) =
                                    (&ast.nodes[*obj as usize], &ast.nodes[*key as usize])
                                {
                                    let base = if let Some(bid) = res.node_bid[*obj as usize] {
                                        analyzer.api_alias_get(bid)
                                    } else {
                                        None
                                    };
                                    base.map(|base| format!("{}.{}", base, decode_lua_string(ksid)))
                                } else {
                                    None
                                }
                            }
                            _ => None,
                        };
                        if let Some(alias) = alias {
                            if analyzer.api_alias_get(tbid) != Some(&alias) {
                                analyzer.api_alias_set(tbid, alias);
                                changed = true;
                            }
                        }
                    }
                }
                if !changed {
                    break;
                }
            }
        }

        // functions / summaries（bid 昇順。TS は bindings Map の挿入順＝bid 順）
        for bid in 1..res.bindings.len() as BindingId {
            if let Some(fn_) = res.bindings[bid as usize].function_node {
                analyzer.functions[bid as usize] = Some(fn_);
            }
        }
        for bid in 1..res.bindings.len() as BindingId {
            if analyzer.functions[bid as usize].is_some() {
                analyzer.summaries[bid as usize] = Some(FunctionSummary {
                    pure: false,
                    total: false,
                    reads: Vec::new(),
                    writes: Vec::new(),
                    calls: false,
                    ordered: false,
                    may_throw: false,
                    stable: true,
                });
            }
        }
        for _ in 0..20 {
            let mut changed = false;
            for bid in 1..res.bindings.len() as BindingId {
                let Some(fn_) = analyzer.functions[bid as usize] else {
                    continue;
                };
                // functions は「関数ノード」を保持（TS の b.functionNode）。本体 block を開く。
                let body = match &ast.nodes[fn_ as usize] {
                    Node::Function(_, _, b) => *b,
                    _ => unreachable!(),
                };
                let e = analyzer.block_effects(body, Some(bid));
                // TS は effectKey のみで変化判定する（pure/total は effectKey の比較対象外）。
                // 初期値と同一の効果なら更新されず、pure=false total=false が残る。
                // summaries は fixpoint 開始前に全関数 bid へ Some を注入済みのため None はない。
                #[expect(
                    clippy::expect_used,
                    reason = "Every function-body binding is initialized before the fixed-point loop; no user callback runs during analysis"
                )]
                let old = analyzer.summaries[bid as usize]
                    .as_ref()
                    .expect("summaries は初期化済み");
                if !old.same_effect_key(&e) {
                    analyzer.summaries[bid as usize] = Some(FunctionSummary::from_effects(e));
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }

        analyzer
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod construction_reuse_tests {
    use super::*;
    fn compare(source: &str) {
        let (ast, root) = storm_lua_syntax::parse_source(source).unwrap();
        let res = crate::resolver::resolve(&ast, root);
        compare_parsed(&ast, &res, root);
    }
    fn compare_parsed(ast: &Ast, res: &Resolution, root: NodeId) {
        for stable in [false, true] {
            let mut a = EffectAnalyzer::reference_new(ast, res, root, stable);
            let mut b = EffectAnalyzer::new(ast, res, root, stable);
            assert_eq!(a.functions, b.functions);
            assert_eq!(a.summaries, b.summaries);
            assert_eq!(a.api_alias_order, b.api_alias_order);
            assert_eq!(a.api_alias_map, b.api_alias_map);
            for setting in [stable, !stable] {
                a.stable_reads = setting;
                b.stable_reads = setting;
                for id in 0..ast.nodes.len() as u32 {
                    assert_eq!(a.effects_for_expr(id), b.effects_for_expr(id));
                    let full = b.effects_for_expr(id);
                    let pure = !full.calls && !full.ordered && full.writes.is_empty();
                    assert_eq!(b.is_pure(id), pure);
                    assert_eq!(b.is_movable(id), pure && !full.may_throw && full.stable);
                    assert_eq!(a.effects_for_statement(id), b.effects_for_statement(id));
                    assert_eq!(
                        a.resolve_builtin_reference(id),
                        b.resolve_builtin_reference(id)
                    );
                }
            }
            // Public summaries remain mutable, so no expression result may be
            // cached across user-visible changes of the analysis state.
            if let Some(i) = a.summaries.iter().position(Option::is_some) {
                a.summaries[i] = None;
                b.summaries[i] = None;
                assert_eq!(a.effects_for_expr(root), b.effects_for_expr(root));
                let full = b.effects_for_expr(root);
                let pure = !full.calls && !full.ordered && full.writes.is_empty();
                assert_eq!(b.is_pure(root), pure);
                assert_eq!(b.is_movable(root), pure && !full.may_throw && full.stable);
            }
        }
    }
    #[test]
    fn alias_order_and_function_fixpoints_match_original_construction() {
        for source in [
            "",
            "function onTick() output.setNumber(1,input.getNumber(1)) output.setNumber(2,input.getNumber(2))end",
            "i=input g=i.getNumber function read()return g(1)end function onTick()output.setNumber(1,read())end",
            "a=b b=c c=d d=e e=f f=g g=h h=input x=a.getNumber function onTick()output.setNumber(1,x(2))end",
            "function a(x)if x then return b(x-1)end end function b(x)return a(x)end function onTick()a(3)end",
            "local function a()return 1 end local function b()return a()end local v=b() function onTick()output.setNumber(1,v)end",
            "input={} local screen={} function f()screen.drawText(0,0,'shadow')end function onTick() f()end",
            "screen=screen function onDraw()screen.drawLine(0,0,1,1)end",
        ] {compare(source);}
        for n in [1, 16, 128] {
            compare(&format!(
                "function onDraw(){}end",
                "screen.drawLine(0,0,1,1) ".repeat(n)
            ));
            compare(&format!(
                "{}function f(x)return x end function onTick()output.setNumber(1,f(2))end",
                (0..n).map(|i| format!("v{i}={i} ")).collect::<String>()
            ));
        }
    }
    #[test]
    fn fused_seeding_preserves_late_writes_shadowing_and_subroot_scope() {
        for source in [
            "i=input o=output input={} function onTick()o.setNumber(1,i.getNumber(1))end",
            "i=input input=input output=output local math={} function f()return i.getNumber(2)end",
            "input={} do local before=input.getNumber(1) output.setNumber(1,before)end",
            "local input={} do i=input end function f()return input.value end",
            "function f()return math.sin(1)end math={} local x=f()",
        ] {
            let (ast, root) = storm_lua_syntax::parse_source(source).unwrap();
            let res = crate::resolver::resolve(&ast, root);
            compare_parsed(&ast, &res, root);
            for (id, node) in ast.nodes.iter().enumerate() {
                if matches!(node, Node::Block(_)) {
                    compare_parsed(&ast, &res, id as NodeId);
                }
            }
        }
    }
    #[test]
    fn shared_child_occurrences_are_counted_without_including_unreachable_nodes() {
        let (mut ast, root) =
            storm_lua_syntax::parse_source("i=input x=i.getNumber(1) output.setNumber(1,x)")
                .unwrap();
        let statements = match ast.node(root) {
            Node::Block(values) => values.clone(),
            _ => panic!("fixture root"),
        };
        let mut repeated = statements.clone();
        repeated.extend_from_slice(&statements);
        ast.nodes[root as usize] = Node::Block(repeated);
        let symbol = ast.strings.intern("input");
        let unreachable = ast.push(Node::Name(symbol));
        let mut res = crate::resolver::resolve(&ast, root);
        // An unrelated arena slot must not become a reachable builtin, even if
        // a low-level caller supplies a resolved binding for it.
        res.node_write[unreachable as usize] = true;
        let input_bid = res
            .bindings
            .iter()
            .position(|binding| {
                binding.kind == BindingKind::Global && ast.strings.get(binding.name) == "input"
            })
            .unwrap() as BindingId;
        res.node_bid[unreachable as usize] = Some(input_bid);
        compare_parsed(&ast, &res, root);
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod borrowed_builtin_tests {
    use super::*;
    use std::borrow::Cow;
    #[test]
    fn split_path_comparisons_preserve_exact_and_prefix_string_semantics() {
        let names = PURE_API
            .iter()
            .chain(TOTAL_API)
            .chain(EFFECT_API)
            .copied()
            .chain([
                "screen.getWidth",
                "screen.getHeight",
                "screen.drawText",
                "screen.",
                "screen..",
                "screen.a.b",
                "screen.雪😀",
                "input.",
                "property.",
                "",
                ".",
            ])
            .collect::<Vec<_>>();
        for &base in &names {
            for member in [
                None,
                Some(""),
                Some("getNumber"),
                Some("sin"),
                Some("getWidth"),
                Some("getHeight"),
                Some("drawText"),
                Some("a.b"),
                Some("雪😀"),
                Some("\0"),
            ] {
                let r = BuiltinReference {
                    base: Cow::Borrowed(base),
                    member: member.map(Cow::Borrowed),
                };
                let text = member.map_or_else(|| base.to_owned(), |key| format!("{base}.{key}"));
                for &name in &names {
                    assert_eq!(r.is(name), text == name, "{text:?} vs {name:?}");
                }
                for prefix in ["screen.", "input.", "property.", "screen", "s", "", "."] {
                    assert_eq!(r.starts_with(prefix), text.starts_with(prefix));
                }
                assert_eq!(r.into_text(), text);
            }
        }
        for raw in [
            "'sin'",
            "\"drawText\"",
            "'雪😀'",
            "'a.b'",
            "'s\\105n'",
            "'\\0'",
            "[=[getWidth]=]",
            "[[\nvalue]]",
            "'a\\z \n b'",
            "'a\\\nb'",
            "unquoted",
        ] {
            assert_eq!(borrowed_member(raw).as_ref(), decode_lua_string(raw));
        }
        assert!(matches!(borrowed_member("'sin'"), Cow::Borrowed(_)));
    }
    #[test]
    fn borrowed_classification_matches_allocating_effects_with_aliases_shadowing_and_summaries() {
        let mut sources=vec![
            "i=input get=i.getNumber function f(v)return math.sin(v)end function onTick()output.setNumber(1,f(get(1)))end".to_owned(),
            "i=input i={} function f()return i.getNumber(1)end".to_owned(),
            "local input={} function f()return input.getNumber(1)end".to_owned(),
            "m=math s=m['s\\105n'] function f(x)return s(x)end".to_owned(),
            "s=screen function f()return s.getWidth()+s.getHeight()end function onDraw()s.drawText(0,0,'a')end".to_owned(),
            "function f()local x=unknown()return table.unpack({1}),string['format']('%s',x)end".to_owned(),
            "function a()return b()end function b()return a()end x=a()".to_owned(),
            "math.foo.bar(1) screen['a.b'](2) screen['getWidth.more'](3) input['']()".to_owned(),
        ];
        for name in PURE_API.iter().chain(TOTAL_API).chain(EFFECT_API).chain(
            [
                "screen.drawText",
                "screen.getWidth",
                "screen.getHeight",
                "math.notBuiltin",
                "input.getNumber.extra",
            ]
            .iter(),
        ) {
            sources.push(format!(
                "function f(x)return {name}(x,1)end function onTick()f(input.getNumber(1))end"
            ));
        }
        for source in sources {
            let (ast, root) = storm_lua_syntax::parse_source(&source).unwrap();
            let res = crate::resolver::resolve(&ast, root);
            let mut analyzer = EffectAnalyzer::new(&ast, &res, root, true);
            for stable in [true, false] {
                analyzer.stable_reads = stable;
                for round in 0..2 {
                    if round == 1 {
                        for summary in analyzer.summaries.iter_mut().flatten() {
                            summary.calls = !summary.calls;
                            summary.may_throw = !summary.may_throw;
                            summary.writes.push(1);
                        }
                    }
                    for (id, _) in ast.nodes.iter().enumerate() {
                        let mut expected = empty_effects();
                        analyzer.allocating_expr_into::<true>(id as NodeId, &mut expected);
                        assert_eq!(
                            analyzer.effects_for_expr(id as NodeId),
                            normalize_effects(expected.clone()),
                            "source={source}, node={id}"
                        );
                        assert_eq!(
                            analyzer.is_pure(id as NodeId),
                            !expected.calls && !expected.ordered && expected.writes.is_empty()
                        );
                        assert_eq!(
                            analyzer.is_movable(id as NodeId),
                            !expected.calls
                                && !expected.ordered
                                && expected.writes.is_empty()
                                && !expected.may_throw
                                && expected.stable
                        );
                    }
                }
            }
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod summary_dependency_tests {
    use super::*;
    fn compare(source: &str) {
        let (ast, root) = storm_lua_syntax::parse_source(source).unwrap();
        let res = crate::resolver::resolve(&ast, root);
        for stable in [false, true] {
            let a = EffectAnalyzer::reference_new(&ast, &res, root, stable);
            let mut b = EffectAnalyzer::new(&ast, &res, root, stable);
            assert_eq!(a.functions, b.functions);
            assert_eq!(
                a.summaries, b.summaries,
                "summary iteration result differs for {source}"
            );
            assert_eq!(a.api_alias_order, b.api_alias_order);
            assert_eq!(a.api_alias_map, b.api_alias_map);
            for id in 0..ast.nodes.len() as NodeId {
                assert_eq!(a.effects_for_expr(id), b.effects_for_expr(id));
                assert_eq!(a.effects_for_statement(id), b.effects_for_statement(id));
            }
            b.stable_reads = !stable;
            // The constructor's dependency state must not survive into mutable
            // public queries, including edits to summaries made by callers.
            for summary in &mut b.summaries {
                *summary = None;
            }
            let mut expected = EffectAnalyzer::reference_new(&ast, &res, root, !stable);
            for summary in &mut expected.summaries {
                *summary = None;
            }
            for id in 0..ast.nodes.len() as NodeId {
                assert_eq!(expected.effects_for_expr(id), b.effects_for_expr(id));
            }
        }
    }
    #[test]
    fn dependent_summaries_keep_order_round_limit_cycles_and_initial_values() {
        for count in [1, 2, 7, 19, 20, 21, 32] {
            for reversed in [false, true] {
                for cycle in [false, true] {
                    let mut declarations = (0..count)
                        .map(|i| {
                            let body = if i + 1 < count {
                                format!("return f{}(x)", i + 1)
                            } else if cycle {
                                "if x>0 then return f0(x-1)end output.setNumber(1,x)".to_string()
                            } else {
                                "output.setNumber(1,input.getNumber(1)) return x".to_string()
                            };
                            format!("function f{i}(x) {body} end ")
                        })
                        .collect::<Vec<_>>();
                    if reversed {
                        declarations.reverse();
                    }
                    compare(&(declarations.concat() + "function onTick()f0(2)end"));
                }
            }
        }
        for source in [
            "function empty()end function a()return empty end function b()return a()end",
            "function a()return b()end function b()return c()end function c()return a()end",
            "function f()local function hidden()return g()end return hidden end function g()return f()end",
            "function f()return 1 end function g()tab[f()]=2 end function h()g()end",
            "g=math.sin function g(x)return h(x)end function h(x)return input.getNumber(x)end function onTick()g(1)end",
            "function a()return 1 end function b(a)return a()end function onTick()b(a)end",
            "function a()return b()end function b()return math.sin(1)end b=input.getNumber function onTick()a()end",
        ] {compare(source);}
    }
    #[test]
    fn summaries_of_multiple_independent_bodies_and_nested_blocks_stay_exact() {
        for n in [1, 8, 48] {
            let mut source = String::new();
            for i in 0..n {
                source.push_str(&format!(
                    "function f{i}(x)do local v=x+{i} if v then output.setNumber(1,v)end end end "
                ));
            }
            source.push_str("function onTick()f0(2)end");
            compare(&source);
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod binding_query_tests {
    use super::*;
    fn verify(ast: &Ast, root: NodeId) {
        let res = crate::resolver::resolve(ast, root);
        for id in 0..ast.nodes.len() as NodeId {
            let mut sequence = Vec::new();
            walk(ast, id, &mut sequence);
            let mut reads = std::collections::HashMap::new();
            let mut writes = std::collections::HashMap::new();
            for &node in &sequence {
                if matches!(ast.node(node), Node::Name(_)) {
                    if let Some(bid) = res.node_bid[node as usize] {
                        *if res.node_write[node as usize] {
                            &mut writes
                        } else {
                            &mut reads
                        }
                        .entry(bid)
                        .or_insert(0) += 1;
                    }
                }
            }
            assert_eq!(count_binding_uses(ast, &res, id), (reads, writes));
            for bid in 0..res.bindings.len() as u32 + 1 {
                let expected = sequence.iter().any(|&node| {
                    matches!(ast.node(node), Node::Name(_))
                        && res.node_bid[node as usize] == Some(bid)
                });
                assert_eq!(contains_binding(ast, &res, id, bid), expected);
            }
        }
    }
    #[test]
    fn allocation_free_queries_preserve_occurrences_subroots_and_absent_bindings() {
        for source in [
            "local x=1 y=x do local x=2 y=x end output.setNumber(1,y)",
            "local t={} local function f(x) for i=1,3 do t[x+i]=x end return t end function onTick()output.setNumber(1,f(1)[1])end",
            "a=1 if input.getBool(1)then a=a+1 else a=0 end ::again:: for k,v in pairs(tab)do tab[k]=v end",
        ] {
            let (mut ast,root)=storm_lua_syntax::parse_source(source).unwrap();verify(&ast,root);
            if let Node::Block(statements)=ast.node(root) {
                let mut repeated=statements.clone();repeated.extend_from_slice(statements);
                ast.nodes[root as usize]=Node::Block(repeated);
            }
            verify(&ast,root);
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod borrowed_alias_propagation_tests {
    use super::*;
    #[test]
    fn unchanged_aliases_preserve_eight_round_limit_order_and_decoded_members() {
        for depth in [1, 2, 7, 8, 9, 16] {
            for reversed in [false, true] {
                for member in [
                    "getNumber",
                    "get\\x4eumber",
                    "get\\078umber",
                    "雪😀",
                    "a.b",
                    "",
                ] {
                    let mut parts = (0..depth)
                        .map(|i| format!("a{i}=a{} ", i + 1))
                        .collect::<Vec<_>>();
                    parts.push(format!("a{depth}=input "));
                    if reversed {
                        parts.reverse();
                    }
                    let source=format!("{} g=a0['{member}'] h=g function f()return h(1)end function onTick()output.setNumber(1,f())end",parts.concat());
                    let (ast, root) = storm_lua_syntax::parse_source(&source).unwrap();
                    let res = crate::resolver::resolve(&ast, root);
                    for stable in [false, true] {
                        let a = EffectAnalyzer::reference_new(&ast, &res, root, stable);
                        let b = EffectAnalyzer::new(&ast, &res, root, stable);
                        assert_eq!(a.api_alias_order, b.api_alias_order);
                        assert_eq!(a.api_alias_map, b.api_alias_map);
                        assert_eq!(a.summaries, b.summaries);
                        for id in 0..ast.nodes.len() as u32 {
                            assert_eq!(a.effects_for_expr(id), b.effects_for_expr(id));
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod builtin_membership_tests {
    use super::*;
    use std::borrow::Cow;
    fn check(reference: &BuiltinReference<'_>) {
        assert_eq!(
            is_pure_api(reference),
            PURE_API.iter().any(|n| reference.is(n))
        );
        assert_eq!(
            is_total_api(reference),
            TOTAL_API.iter().any(|n| reference.is(n))
        );
        assert_eq!(
            is_effect_api(reference),
            EFFECT_API.iter().any(|n| reference.is(n))
        );
    }
    #[test]
    fn exact_component_matches_cover_aliases_nested_paths_and_lookalikes() {
        let mut values = PURE_API
            .iter()
            .chain(TOTAL_API)
            .chain(EFFECT_API)
            .map(|s| s.to_string())
            .collect::<Vec<_>>();
        for v in [
            "",
            "screen.getWidth",
            "screen.getHeight",
            "screen.drawText",
            "custom.input.getNumber",
            "math.sin.extra",
            "math.",
            ".sin",
            "雪",
            "math.雪",
            "math..sin",
            "input.getNumber\0",
            "Type",
            "type.",
        ] {
            values.push(v.into());
        }
        for value in &values {
            for text in [
                value.clone(),
                format!("x{value}"),
                format!("{value}x"),
                value.to_uppercase(),
            ] {
                check(&BuiltinReference {
                    base: Cow::Borrowed(&text),
                    member: None,
                });
                for (offset, ch) in text.char_indices().filter(|(_, c)| *c == '.') {
                    assert_eq!(ch, '.');
                    check(&BuiltinReference {
                        base: Cow::Borrowed(&text[..offset]),
                        member: Some(Cow::Borrowed(&text[offset + 1..])),
                    });
                }
            }
        }
        for root in [
            "input",
            "math",
            "property",
            "screen",
            "table",
            "string",
            "",
            "custom",
            "input.getNumber",
        ] {
            for member in values.iter().map(String::as_str).chain([
                "getNumber",
                "getBool",
                "sin",
                "sqrt",
                "floor",
                "abs",
                "getText",
                "getWidth",
                "setNumber",
                "pack",
                "unpack",
                "format",
            ]) {
                check(&BuiltinReference {
                    base: Cow::Borrowed(root),
                    member: Some(Cow::Borrowed(member)),
                });
            }
        }
    }
    #[test]
    fn public_lists_remain_in_the_existing_order() {
        assert_eq!(
            PURE_API,
            [
                "math.pi",
                "math.sin",
                "math.cos",
                "math.tan",
                "math.atan",
                "math.sqrt",
                "math.abs",
                "math.max",
                "math.min",
                "math.floor",
                "input.getNumber",
                "input.getBool",
                "property.getNumber",
                "property.getBool",
                "string.pack",
                "string.unpack",
                "string.format",
                "table.unpack",
                "type",
                "tonumber",
                "tostring"
            ]
        );
        assert_eq!(
            TOTAL_API,
            [
                "input.getNumber",
                "input.getBool",
                "property.getNumber",
                "property.getBool",
                "math.abs",
                "math.max",
                "math.min",
                "math.floor",
                "math.sin",
                "math.cos",
                "math.tan",
                "math.atan",
                "type",
                "tostring"
            ]
        );
        assert_eq!(EFFECT_API, ["output.setNumber", "output.setBool"]);
    }
}
