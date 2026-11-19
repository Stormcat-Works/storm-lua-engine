//! Canonical lexical resolver。Resolution は AST と別テーブルで保持する。
//!
//! Ast は純粋のまま、bid/bids/scopeId/write を Resolution の並列配列に記録する（D-29）。
//! TS 実装と同一の手順（pos カウンタ・env スタック・scope 生成順・group 割当）を踏むため、
//! binding の decl / last / freq / group と scope の children / refs が byte 一致する。
//!
//! Dense symbol slots plus a scope undo log provide constant-time lookup.
//! A repeat body's bindings remain active through its until condition; function
//! and loop scopes restore the previous slots after resolving their bodies.

use storm_lua_syntax::ast::{Ast, Node, NodeId, SymbolId};

pub type BindingId = u32;
pub type ScopeId = u32;

struct Env {
    bindings: Vec<Option<BindingId>>,
    undo: Vec<(SymbolId, Option<BindingId>)>,
}

impl Env {
    fn new(symbols: usize) -> Self {
        Self {
            bindings: vec![None; symbols],
            undo: Vec::new(),
        }
    }
    fn bind(&mut self, name: SymbolId, bid: BindingId) {
        let previous = self.bindings[name.0 as usize].replace(bid);
        self.undo.push((name, previous));
    }
    fn checkpoint(&self) -> usize {
        self.undo.len()
    }
    fn restore(&mut self, checkpoint: usize) {
        for (name, previous) in self.undo.drain(checkpoint..).rev() {
            self.bindings[name.0 as usize] = previous;
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BindingKind {
    Global,
    Local,
    Param,
    Localfunc,
    For,
}

impl BindingKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            BindingKind::Global => "global",
            BindingKind::Local => "local",
            BindingKind::Param => "param",
            BindingKind::Localfunc => "localfunc",
            BindingKind::For => "for",
        }
    }
}

#[derive(Clone, Debug)]
pub struct Binding {
    pub name: SymbolId,
    pub kind: BindingKind,
    pub scope: ScopeId,
    pub fixed: bool,
    pub decl: u32,
    pub last: u32,
    pub freq: u32,
    pub group: u32,
    pub function_node: Option<NodeId>,
    /// TS の binding.declNode 相当（Phase 4 パスが参照）。param では関数ノード、
    /// local/for 系では宣言ステートメント、global では未設定。
    pub decl_node: Option<NodeId>,
}

#[derive(Clone, Debug)]
pub struct Scope {
    pub id: ScopeId,
    pub parent: Option<ScopeId>,
    pub bindings: Vec<BindingId>,
    pub children: Vec<ScopeId>,
    pub refs: Vec<BindingId>,
}

/// パスが参照する解決結果（別テーブル）。各配列の添字は NodeId。
#[derive(Debug)]
pub struct Resolution {
    pub bindings: Vec<Binding>,              // index = bid（0 は未使用）
    pub scopes: Vec<Scope>,                  // index = sid
    pub globals: Vec<(SymbolId, BindingId)>, // 挿入順（決定性）
    pub node_scope_id: Vec<Option<ScopeId>>,
    pub node_bid: Vec<Option<BindingId>>,
    pub node_bids: Vec<Vec<BindingId>>,
    pub node_write: Vec<bool>,
    pub binding_write_counts: Vec<u32>,
}

impl Resolution {
    pub fn binding(&self, bid: BindingId) -> &Binding {
        &self.bindings[bid as usize]
    }
    pub fn scope(&self, sid: ScopeId) -> &Scope {
        &self.scopes[sid as usize]
    }
}

/// Stormworks ランタイムがスクリプト側の write なしに提供するグローバル名。
pub use storm_lua_spec::environment::VEHICLE_ROOTS as API_ROOTS;

pub fn api_roots(name: &str) -> bool {
    API_ROOTS.contains(&name)
}

/// Component Mod プロファイルが提供するグローバル名（spec の COMPONENT_ROOTS が正本）。
pub use storm_lua_spec::environment::COMPONENT_ROOTS;

pub fn component_api_roots(name: &str) -> bool {
    COMPONENT_ROOTS.contains(&name)
}

/// RESERVED = KEYWORDS + 車両/Component のコールバック名。
pub fn reserved(name: &str) -> bool {
    matches!(
        name,
        "and"
            | "break"
            | "do"
            | "else"
            | "elseif"
            | "end"
            | "false"
            | "for"
            | "function"
            | "goto"
            | "if"
            | "in"
            | "local"
            | "nil"
            | "not"
            | "or"
            | "repeat"
            | "return"
            | "then"
            | "true"
            | "until"
            | "while"
    ) || storm_lua_spec::environment::VEHICLE_CALLBACKS.contains(&name)
        || storm_lua_spec::environment::COMPONENT_CALLBACKS.contains(&name)
}

/// TS の forEachChild と同じ順で子ノードを訪問する（キー挿入順。annotate skip は無視）。
/// 順序は `pos` カウンタ（decl/last/freq/refs）を左右するため、parser の構築順と一致させる。
pub use storm_lua_syntax::ast_utils::for_each_child;

struct Ctx<'a> {
    ast: &'a Ast,
    bindings: Vec<Binding>,
    scopes: Vec<Scope>,
    globals: Vec<(SymbolId, BindingId)>,
    global_lookup: Vec<Option<BindingId>>,
    node_scope_id: Vec<Option<ScopeId>>,
    node_bid: Vec<Option<BindingId>>,
    node_bids: Vec<Vec<BindingId>>,
    node_write: Vec<bool>,
    next_bid: BindingId,
    next_sid: ScopeId,
    pos: u32,
    next_group: u32,
}

impl<'a> Ctx<'a> {
    fn new(ast: &'a Ast) -> Self {
        let n = ast.nodes.len();
        Self {
            ast,
            bindings: vec![Binding {
                name: storm_lua_syntax::ast::SymbolId(0),
                kind: BindingKind::Global,
                scope: 0,
                fixed: false,
                decl: 0,
                last: 0,
                freq: 0,
                group: 0,
                function_node: None,
                decl_node: None,
            }],
            scopes: vec![Scope {
                id: 0,
                parent: None,
                bindings: Vec::new(),
                children: Vec::new(),
                refs: Vec::new(),
            }],
            globals: Vec::new(),
            global_lookup: vec![None; ast.strings.symbol_count()],
            node_scope_id: vec![None; n],
            node_bid: vec![None; n],
            node_bids: vec![Vec::new(); n],
            node_write: vec![false; n],
            next_bid: 1,
            next_sid: 1,
            pos: 0,
            next_group: 1,
        }
    }

    fn new_scope(&mut self, parent: ScopeId) -> ScopeId {
        let id = self.next_sid;
        self.next_sid += 1;
        self.scopes.push(Scope {
            id,
            parent: Some(parent),
            bindings: Vec::new(),
            children: Vec::new(),
            refs: Vec::new(),
        });
        self.scopes[parent as usize].children.push(id);
        id
    }

    fn group(&mut self) -> u32 {
        let g = self.next_group;
        self.next_group += 1;
        g
    }

    fn bind(
        &mut self,
        name: SymbolId,
        scope: ScopeId,
        kind: BindingKind,
        fixed: bool,
        group: u32,
        decl_node: Option<NodeId>,
    ) -> BindingId {
        let id = self.next_bid;
        self.next_bid += 1;
        self.pos += 1;
        self.bindings.push(Binding {
            name,
            kind,
            scope,
            fixed,
            decl: self.pos,
            last: self.pos,
            freq: 0,
            group,
            function_node: None,
            decl_node,
        });
        self.scopes[scope as usize].bindings.push(id);
        id
    }

    fn global_bid(&mut self, name: SymbolId) -> BindingId {
        if let Some(id) = self.global_lookup[name.0 as usize] {
            return id;
        }
        let s = self.ast.strings.get(name);
        let g = self.group();
        let id = self.bind(
            name,
            0,
            BindingKind::Global,
            reserved(s) || api_roots(s),
            g,
            None,
        );
        self.globals.push((name, id));
        self.global_lookup[name.0 as usize] = Some(id);
        id
    }

    fn lookup(&mut self, name: SymbolId, env: &Env) -> BindingId {
        if let Some(bid) = env.bindings[name.0 as usize] {
            return bid;
        }
        self.global_bid(name)
    }

    fn mark_name(&mut self, x: NodeId, env: &Env, scope: ScopeId, write: bool) {
        self.pos += 1;
        let name = match &self.ast.nodes[x as usize] {
            Node::Name(s) => *s,
            _ => unreachable!(),
        };
        let bid = self.lookup(name, env);
        self.node_bid[x as usize] = Some(bid);
        self.node_write[x as usize] = write;
        self.bindings[bid as usize].freq += 1;
        let p = self.pos;
        if p > self.bindings[bid as usize].last {
            self.bindings[bid as usize].last = p;
        }
        self.scopes[scope as usize].refs.push(bid);
    }

    fn expression(&mut self, x: NodeId, env: &mut Env, scope: ScopeId) {
        if matches!(self.ast.nodes[x as usize], Node::Name(_)) {
            self.mark_name(x, env, scope, false);
            return;
        }
        if let Node::Function(..) = self.ast.nodes[x as usize] {
            self.function_node(x, env, scope);
            return;
        }
        let ast = self.ast;
        for_each_child(ast, x, &mut |q| self.expression(q, env, scope));
    }

    fn lvalue(&mut self, x: NodeId, env: &mut Env, scope: ScopeId) {
        if matches!(self.ast.nodes[x as usize], Node::Name(_)) {
            self.mark_name(x, env, scope, true);
        } else {
            self.expression(x, env, scope);
        }
    }

    fn function_node(&mut self, fn_: NodeId, env: &mut Env, parent: ScopeId) {
        let sid = self.new_scope(parent);
        let checkpoint = env.checkpoint();
        let mut bids: Vec<BindingId> = Vec::new();
        let group = self.group();
        let (ps, body) = match &self.ast.nodes[fn_ as usize] {
            Node::Function(ps, _, b) => (ps, *b),
            _ => unreachable!(),
        };
        for &p in ps {
            let bid = self.bind(p, sid, BindingKind::Param, false, group, Some(fn_));
            env.bind(p, bid);
            bids.push(bid);
        }
        self.node_bids[fn_ as usize] = bids;
        self.node_scope_id[fn_ as usize] = Some(sid);
        self.block_node(body, env, sid, false);
        env.restore(checkpoint);
    }

    fn block_node(&mut self, block: NodeId, env: &mut Env, parent: ScopeId, new_scope: bool) {
        let sid = if new_scope {
            self.new_scope(parent)
        } else {
            parent
        };
        self.node_scope_id[block as usize] = Some(sid);
        let checkpoint = env.checkpoint();
        let ss = match &self.ast.nodes[block as usize] {
            Node::Block(ss) => ss,
            _ => unreachable!(),
        };
        for &s in ss {
            match &self.ast.nodes[s as usize] {
                Node::Local(names, es) => {
                    for &e in es {
                        self.expression(e, env, sid);
                    }
                    let mut bids: Vec<BindingId> = Vec::new();
                    let g = self.group();
                    for &name in names {
                        let bid = self.bind(name, sid, BindingKind::Local, false, g, Some(s));
                        env.bind(name, bid);
                        bids.push(bid);
                    }
                    self.node_bids[s as usize] = bids;
                }
                Node::Localfunc(name, fn_) => {
                    let g = self.group();
                    let bid = self.bind(*name, sid, BindingKind::Localfunc, false, g, Some(s));
                    env.bind(*name, bid);
                    self.node_bid[s as usize] = Some(bid);
                    self.bindings[bid as usize].function_node = Some(*fn_);
                    self.function_node(*fn_, env, sid);
                }
                Node::Assign(vs, es) => {
                    for &e in es {
                        self.expression(e, env, sid);
                    }
                    for &v in vs {
                        self.lvalue(v, env, sid);
                    }
                }
                Node::Callstat(e) => self.expression(*e, env, sid),
                Node::Return(es) => {
                    for &e in es {
                        self.expression(e, env, sid);
                    }
                }
                Node::Funcstat(target, fn_) => {
                    self.lvalue(*target, env, sid);
                    if matches!(self.ast.nodes[*target as usize], Node::Name(_)) {
                        if let Some(bid) = self.node_bid[*target as usize] {
                            self.bindings[bid as usize].function_node = Some(*fn_);
                            self.bindings[bid as usize].decl_node = Some(s);
                        }
                    }
                    self.function_node(*fn_, env, sid);
                }
                Node::If(arms, eb) => {
                    for arm in arms {
                        self.expression(arm.cond, env, sid);
                        self.block_node(arm.body, env, sid, true);
                    }
                    if let Some(e) = eb {
                        self.block_node(*e, env, sid, true);
                    }
                }
                Node::While(e, b) => {
                    self.expression(*e, env, sid);
                    self.block_node(*b, env, sid, true);
                }
                Node::Repeat(b, e) => {
                    let csid = self.new_scope(sid);
                    let checkpoint = env.checkpoint();
                    self.node_scope_id[*b as usize] = Some(csid);
                    self.block_node(*b, env, csid, false);
                    self.expression(*e, env, csid);
                    env.restore(checkpoint);
                }
                Node::Do(b) => {
                    self.block_node(*b, env, sid, true);
                }
                Node::Fornum(name, a, b, c, body) => {
                    self.expression(*a, env, sid);
                    self.expression(*b, env, sid);
                    if let Some(c) = c {
                        self.expression(*c, env, sid);
                    }
                    let csid = self.new_scope(sid);
                    let checkpoint = env.checkpoint();
                    let g = self.group();
                    let bid = self.bind(*name, csid, BindingKind::For, false, g, Some(s));
                    env.bind(*name, bid);
                    self.node_bid[s as usize] = Some(bid);
                    self.node_scope_id[s as usize] = Some(csid);
                    self.block_node(*body, env, csid, false);
                    env.restore(checkpoint);
                }
                Node::Forin(names, es, body) => {
                    for &e in es {
                        self.expression(e, env, sid);
                    }
                    let csid = self.new_scope(sid);
                    let checkpoint = env.checkpoint();
                    let mut bids: Vec<BindingId> = Vec::new();
                    let g = self.group();
                    for &name in names {
                        let bid = self.bind(name, csid, BindingKind::For, false, g, Some(s));
                        env.bind(name, bid);
                        bids.push(bid);
                    }
                    self.node_bids[s as usize] = bids;
                    self.node_scope_id[s as usize] = Some(csid);
                    self.block_node(*body, env, csid, false);
                    env.restore(checkpoint);
                }
                _ => {}
            }
        }
        if new_scope {
            env.restore(checkpoint);
        }
    }
}

/// 与えられた root に対する Resolution を計算する。
pub fn resolve(ast: &Ast, root: NodeId) -> Resolution {
    let mut ctx = Ctx::new(ast);
    let mut env0 = Env::new(ast.strings.symbol_count());
    ctx.block_node(root, &mut env0, 0, false);
    let mut binding_write_counts = vec![0u32; ctx.bindings.len()];
    // 不変条件: resolve は常に arena 全体（root が全ノードを内包）を対象にする。
    // ここは root からの walk ではなく arena 全体の node_write を走査するが、parity
    // 上は同等（effects.ts:117 の walk(root) と同じ件数になる）。sub-tree を solve
    // する用法は現状想定していない旨を残す。
    for i in 0..ctx.node_write.len() {
        if ctx.node_write[i] {
            if let Some(b) = ctx.node_bid[i] {
                binding_write_counts[b as usize] += 1;
            }
        }
    }
    Resolution {
        bindings: ctx.bindings,
        scopes: ctx.scopes,
        globals: ctx.globals,
        node_scope_id: ctx.node_scope_id,
        node_bid: ctx.node_bid,
        node_bids: ctx.node_bids,
        node_write: ctx.node_write,
        binding_write_counts,
    }
}
