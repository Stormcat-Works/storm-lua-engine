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

pub const PURE_API: &[&str] = &[
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
    "tostring",
];

pub const TOTAL_API: &[&str] = &[
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
    "tostring",
];

pub const EFFECT_API: &[&str] = &["output.setNumber", "output.setBool"];

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
                if b.kind == BindingKind::Global
                    && ast.strings.get(b.name) == name
                    && analyzer.api_alias_get(bid).is_none()
                {
                    // The first occurrence establishes this immutable root.
                    // Repeated reads previously allocated the identical String.
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
        let mut function_bindings = Vec::new();
        for (bid, binding) in res.bindings.iter().enumerate().skip(1) {
            if let Some(fn_) = binding.function_node {
                analyzer.functions[bid] = Some(fn_);
                function_bindings.push((bid as BindingId, fn_));
            }
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
        for _ in 0..20 {
            let mut changed = false;
            // The worklist retains binding order, round count and immediate
            // summary updates. Non-function bindings cannot enter this fixpoint.
            for &(bid, fn_) in &function_bindings {
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

    fn api_alias_get(&self, bid: BindingId) -> Option<&String> {
        self.api_alias_map[bid as usize].as_ref()
    }
    fn api_alias_set(&mut self, bid: BindingId, alias: String) {
        if self.api_alias_map[bid as usize].is_none() {
            self.api_alias_order.push((bid, alias.clone()));
        }
        self.api_alias_map[bid as usize] = Some(alias);
    }

    /// 証明済み single-write エイリアスのみを通してビルトイン root/member を解決する。
    pub fn resolve_builtin_reference(&self, x: NodeId) -> Option<String> {
        match &self.ast.nodes[x as usize] {
            Node::Name(_) => {
                let bid = self.res.node_bid[x as usize]?;
                self.api_alias_get(bid).cloned()
            }
            Node::Index(obj, key, _) => {
                if let Node::Str(s) = &self.ast.nodes[*key as usize] {
                    let base = self.resolve_builtin_reference(*obj)?;
                    Some(format!("{}.{}", base, decode_lua_string(s)))
                } else {
                    None
                }
            }
            _ => None,
        }
    }

    // A single accumulator avoids a Vec<Effects> and two binding-vector
    // allocations per expression. No results are cached across mutable summary
    // fixpoint iterations or changes to the public stable_reads setting.
    fn expr_into(&self, x: NodeId, e: &mut Effects) {
        match &self.ast.nodes[x as usize] {
            Node::Name(_) => {
                if let Some(bid) = self.res.node_bid[x as usize] {
                    e.reads.push(bid);
                }
            }
            Node::Num(_)
            | Node::Str(_)
            | Node::Nil
            | Node::Bool(_)
            | Node::Vararg
            | Node::Function(..) => {}
            Node::Paren(x) => self.expr_into(*x, e),
            Node::Table(fs) => {
                for f in fs {
                    match f {
                        storm_lua_syntax::ast::TableField::Arr(v)
                        | storm_lua_syntax::ast::TableField::Name(_, v) => {
                            self.expr_into(*v, e);
                        }
                        storm_lua_syntax::ast::TableField::KVar(k, v) => {
                            self.expr_into(*k, e);
                            self.expr_into(*v, e);
                        }
                    }
                }
            }
            Node::Index(obj, key, _) => {
                self.expr_into(*obj, e);
                self.expr_into(*key, e);
                e.may_throw = true;
            }
            Node::Un(op, x) => {
                self.expr_into(*x, e);
                e.may_throw |= matches!(op.as_str(), "-" | "#" | "~");
            }
            Node::Bin(op, l, r) => {
                self.expr_into(*l, e);
                self.expr_into(*r, e);
                e.may_throw |= !matches!(op.as_str(), "and" | "or" | "==" | "~=");
            }
            Node::Call(function, args, _) => {
                let full = self.resolve_builtin_reference(*function);
                // A proven builtin's callee lookup is total. Only suppress the
                // callee's throw bit, never a previous sibling's or argument's.
                let preceding_throw = e.may_throw;
                self.expr_into(*function, e);
                if full.is_some() {
                    e.may_throw = preceding_throw;
                }
                for &arg in args {
                    self.expr_into(arg, e);
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
                        e.reads.extend_from_slice(&s.reads);
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
            _ => for_each_child(self.ast, x, &mut |q| self.expr_into(q, e)),
        }
    }

    fn statement_into(&self, statement: NodeId, e: &mut Effects) {
        match &self.ast.nodes[statement as usize] {
            Node::Local(_, es) | Node::Return(es) => {
                for &x in es {
                    self.expr_into(x, e);
                }
            }
            Node::Assign(vs, es) => {
                for &x in es {
                    self.expr_into(x, e);
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
                        self.expr_into(v, &mut target);
                        e.reads.extend(target.reads);
                        e.calls |= target.calls;
                        e.may_throw = true;
                    }
                }
            }
            Node::Callstat(x) => self.expr_into(*x, e),
            Node::If(arms, otherwise) => {
                for arm in arms {
                    self.expr_into(arm.cond, e);
                    self.block_into(arm.body, e);
                }
                if let Some(body) = otherwise {
                    self.block_into(*body, e);
                }
            }
            Node::While(condition, body) => {
                self.expr_into(*condition, e);
                self.block_into(*body, e);
            }
            Node::Repeat(body, condition) => {
                self.block_into(*body, e);
                self.expr_into(*condition, e);
            }
            Node::Do(body) => self.block_into(*body, e),
            Node::Fornum(_, a, b, c, body) => {
                self.expr_into(*a, e);
                self.expr_into(*b, e);
                if let Some(c) = c {
                    self.expr_into(*c, e);
                }
                self.block_into(*body, e);
            }
            Node::Forin(_, es, body) => {
                for &x in es {
                    self.expr_into(x, e);
                }
                self.block_into(*body, e);
            }
            _ => {}
        }
    }

    fn block_into(&self, block: NodeId, e: &mut Effects) {
        let Node::Block(statements) = &self.ast.nodes[block as usize] else {
            unreachable!()
        };
        for &statement in statements {
            self.statement_into(statement, e);
        }
    }

    fn expr_effects(&self, x: NodeId, _current_fn: Option<BindingId>) -> Effects {
        let mut e = empty_effects();
        self.expr_into(x, &mut e);
        normalize_effects(e)
    }

    fn statement_effects(&self, statement: NodeId, _current_fn: Option<BindingId>) -> Effects {
        let mut e = empty_effects();
        self.statement_into(statement, &mut e);
        normalize_effects(e)
    }

    fn block_effects(&self, block: NodeId, _current_fn: Option<BindingId>) -> Effects {
        let mut e = empty_effects();
        self.block_into(block, &mut e);
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
        let e = self.expr_effects(x, None);
        !e.calls && !e.ordered && e.writes.is_empty()
    }

    pub fn is_movable(&self, x: NodeId) -> bool {
        let e = self.expr_effects(x, None);
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
    storm_lua_syntax::dump::dump(ast, a) == storm_lua_syntax::dump::dump(ast, b)
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
    let mut nodes = Vec::new();
    walk(ast, id, &mut nodes);
    for &nid in &nodes {
        if let Node::Name(_) = ast.nodes[nid as usize] {
            if let Some(bid) = res.node_bid[nid as usize] {
                let m = if res.node_write[nid as usize] {
                    &mut writes
                } else {
                    &mut reads
                };
                *m.entry(bid).or_insert(0) += 1;
            }
        }
    }
    (reads, writes)
}

/// `containsBinding`: サブツリー内に bid への参照が存在するか。
pub fn contains_binding(ast: &Ast, res: &Resolution, id: NodeId, bid: u32) -> bool {
    let mut nodes = Vec::new();
    walk(ast, id, &mut nodes);
    for &nid in &nodes {
        if let Node::Name(_) = ast.nodes[nid as usize] {
            if res.node_bid[nid as usize] == Some(bid) {
                return true;
            }
        }
    }
    false
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
        for stable in [false, true] {
            let mut a = EffectAnalyzer::reference_new(&ast, &res, root, stable);
            let mut b = EffectAnalyzer::new(&ast, &res, root, stable);
            assert_eq!(a.functions, b.functions);
            assert_eq!(a.summaries, b.summaries);
            assert_eq!(a.api_alias_order, b.api_alias_order);
            assert_eq!(a.api_alias_map, b.api_alias_map);
            for setting in [stable, !stable] {
                a.stable_reads = setting;
                b.stable_reads = setting;
                for id in 0..ast.nodes.len() as u32 {
                    assert_eq!(a.effects_for_expr(id), b.effects_for_expr(id));
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
}
