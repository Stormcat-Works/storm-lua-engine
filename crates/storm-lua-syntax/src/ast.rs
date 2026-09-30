//! Arena-backed Lua syntax. Node and symbol IDs belong to a particular AST.
//! Low-level builders require valid child IDs and expression/statement shapes;
//! ordinary hosts should submit source text through the parser or build API.

// IR 設計（D-29 A-1/A-2/A-3）:
// - enum Node（30 種、parser の t のみ。pass は新種を生成しない — 検証済み）
// - NodeId(u32) arena。clone 撤廃
// - 識別子のみ Interner。str/num リテラルは原文 String のまま保持（f64 化しない）
// - メタ情報（bid/bids/scopeId/write）は Node に持たせず別テーブル Resolution（Phase 3）へ
// - 2 次元子（if.arms / table.fs）は Vec に正規化

use std::collections::HashMap;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

/// Index of a node in its owning arena; not portable across unrelated ASTs.
pub type NodeId = u32;

/// Index of an interned identifier; literal text is stored separately.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub struct SymbolId(
    /// Zero-based index into the owning identifier registry.
    pub u32,
);

/// 識別子のみの interner。pass 実行中に伸びる（scopeRename が新名を生成するため、read-only 文字列スタックでよい）。
#[derive(Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Interner {
    map: Arc<HashMap<Arc<str>, SymbolId>>,
    strings: Arc<Vec<Arc<str>>>,
}

impl Interner {
    /// Create an empty arena or identifier registry.
    pub fn new() -> Self {
        Self {
            map: Arc::new(HashMap::new()),
            strings: Arc::new(Vec::new()),
        }
    }
    /// Return an existing identifier ID or append a new one using copy-on-write storage.
    pub fn intern(&mut self, s: &str) -> SymbolId {
        if let Some(&id) = self.map.get(s) {
            return id;
        }
        let id = SymbolId(self.strings.len() as u32);
        let text: Arc<str> = Arc::from(s);
        Arc::make_mut(&mut self.map).insert(Arc::clone(&text), id);
        Arc::make_mut(&mut self.strings).push(text);
        id
    }
    /// Borrow an identifier. Panics if the ID does not belong to this registry.
    pub fn get(&self, id: SymbolId) -> &str {
        &self.strings[id.0 as usize]
    }
    /// Return the number of registered identifiers without allocating.
    pub fn symbol_count(&self) -> usize {
        self.strings.len()
    }
    /// Query the existing registry without copying every identifier.
    pub fn contains(&self, name: &str) -> bool {
        self.map.contains_key(name)
    }
    /// Preserve insertion-order equality without materializing owned strings.
    pub fn same_symbols(&self, other: &Self) -> bool {
        self.strings == other.strings
    }
    /// Borrow identifiers in insertion order when no owned snapshot is needed.
    pub fn iter(&self) -> impl ExactSizeIterator<Item = &str> {
        self.strings.iter().map(AsRef::as_ref)
    }
    /// Copy identifiers in insertion order for diagnostics and snapshots.
    pub fn all_strings(&self) -> Vec<String> {
        self.strings.iter().map(|s| s.to_string()).collect()
    }
}

/// A conditional expression paired with its branch block.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IfArm {
    /// Condition expression node.
    pub cond: NodeId,
    /// Block executed when the condition is truthy.
    pub body: NodeId,
}

/// table.fs の正規化。TS の ['arr', null, v] / ['name', k, v] / ['kv', k, v] に対応。
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TableField {
    /// Array-style value field.
    Arr(NodeId),
    /// Named field: interned key followed by value.
    Name(SymbolId, NodeId),
    /// Computed field: key expression followed by value expression.
    KVar(NodeId, NodeId),
}

/// ステートメント 16 種 + 式 14 種 = 30 種（D-28a）。
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Node {
    // ---- statements ----
    /// Ordered statement block.
    Block(Vec<NodeId>),
    /// Break from the innermost loop.
    Break,
    /// Jump to an interned label.
    Goto(SymbolId),
    /// Declare an interned label.
    Label(SymbolId),
    /// Scoped block.
    Do(NodeId),
    /// Condition expression followed by loop body.
    While(NodeId, NodeId),
    /// Loop body followed by its terminating condition.
    Repeat(NodeId, NodeId),
    /// Conditional arms followed by an optional else block.
    If(Vec<IfArm>, Option<NodeId>),
    /// Numeric loop: binding, start, end, optional step, and body.
    Fornum(SymbolId, NodeId, NodeId, Option<NodeId>, NodeId),
    /// Generic loop: bindings, iterator expressions, and body.
    Forin(Vec<SymbolId>, Vec<NodeId>, NodeId),
    /// Function declaration: target expression and function value.
    Funcstat(NodeId, NodeId),
    /// Recursive local function: binding and function value.
    Localfunc(SymbolId, NodeId),
    /// Local bindings and their initializer expressions.
    Local(Vec<SymbolId>, Vec<NodeId>),
    /// Return expression list with Lua value-adjustment semantics.
    Return(Vec<NodeId>),
    /// Call used as a statement.
    Callstat(NodeId),
    /// Assignment targets and source expression list.
    Assign(Vec<NodeId>, Vec<NodeId>),
    // ---- expressions ----
    /// Nil literal.
    Nil,
    /// Boolean literal.
    Bool(bool),
    /// Variadic argument expression.
    Vararg,
    /// Original numeric literal spelling, not a rounded host float.
    Num(Arc<str>),
    /// Original Lua string literal spelling.
    Str(Arc<str>),
    /// Parameter bindings, variadic flag, and body.
    Function(Vec<SymbolId>, bool, NodeId),
    /// Table constructor fields in evaluation order.
    Table(Vec<TableField>),
    /// Unary operator and operand.
    Un(String, NodeId),
    /// Binary operator, left operand, and right operand.
    Bin(String, NodeId, NodeId),
    /// Parenthesized expression, including single-value adjustment.
    Paren(NodeId),
    /// Function expression, arguments, and optional method name.
    Call(NodeId, Vec<NodeId>, Option<String>),
    /// Identifier reference.
    Name(SymbolId),
    /// Object, key, and original dot-syntax flag.
    Index(NodeId, NodeId, bool),
    /// Object and method identifier used in declarations.
    Methodname(NodeId, SymbolId),
}

impl Node {
    /// Whether this node is a function or method call expression.
    pub fn is_call(&self) -> bool {
        matches!(self, Node::Call(..))
    }
}

/// Arena-backed syntax. Optional origins live in a separate slot table and are excluded from syntax equality.
#[derive(Clone, PartialEq, Eq)]
pub struct Ast {
    /// Node arena. Structural edits must preserve valid child IDs and node kinds.
    pub nodes: crate::node_arena::NodeArena,
    /// Identifier registry shared by the nodes in this arena.
    pub strings: Interner,
}

impl Ast {
    /// Create an empty arena or identifier registry.
    pub fn new() -> Self {
        Self {
            nodes: crate::node_arena::NodeArena::default(),
            strings: Interner::new(),
        }
    }
    /// Append a structurally valid node and return its arena ID.
    pub fn push(&mut self, node: Node) -> NodeId {
        let id = self.nodes.len() as NodeId;
        self.nodes.push(node);
        id
    }
    /// Borrow an arena node. Panics if the ID is outside this arena.
    pub fn node(&self, id: NodeId) -> &Node {
        &self.nodes[id as usize]
    }
    fn sym(&mut self, s: &str) -> SymbolId {
        self.strings.intern(s)
    }

    // ---- node constructors（TS の N() 相当）----
    /// Append an ordered block of statements.
    pub fn block(&mut self, ss: Vec<NodeId>) -> NodeId {
        self.push(Node::Block(ss))
    }
    /// Append a break statement.
    pub fn break_(&mut self) -> NodeId {
        self.push(Node::Break)
    }
    /// Append a jump to the named label.
    pub fn goto(&mut self, name: &str) -> NodeId {
        let s = self.sym(name);
        self.push(Node::Goto(s))
    }
    /// Append a declaration of the named label.
    pub fn label(&mut self, name: &str) -> NodeId {
        let s = self.sym(name);
        self.push(Node::Label(s))
    }
    /// Append a scoped do block.
    pub fn do_(&mut self, b: NodeId) -> NodeId {
        self.push(Node::Do(b))
    }
    /// Append a while loop with condition and body.
    pub fn while_(&mut self, e: NodeId, b: NodeId) -> NodeId {
        self.push(Node::While(e, b))
    }
    /// Append a repeat-until loop with body and condition.
    pub fn repeat(&mut self, b: NodeId, e: NodeId) -> NodeId {
        self.push(Node::Repeat(b, e))
    }
    /// Append conditional arms and an optional else block.
    pub fn if_(&mut self, arms: Vec<IfArm>, eb: Option<NodeId>) -> NodeId {
        self.push(Node::If(arms, eb))
    }
    /// Append a numeric loop; a missing step uses the Lua default.
    pub fn fornum(
        &mut self,
        name: &str,
        a: NodeId,
        b: NodeId,
        c: Option<NodeId>,
        body: NodeId,
    ) -> NodeId {
        let s = self.sym(name);
        self.push(Node::Fornum(s, a, b, c, body))
    }
    /// Append a generic loop with iterator expressions and local bindings.
    pub fn forin(&mut self, names: Vec<String>, es: Vec<NodeId>, body: NodeId) -> NodeId {
        let s: Vec<SymbolId> = names.iter().map(|n| self.sym(n)).collect();
        self.push(Node::Forin(s, es, body))
    }
    /// Append a function declaration for the supplied target.
    pub fn funcstat(&mut self, target: NodeId, fn_: NodeId) -> NodeId {
        self.push(Node::Funcstat(target, fn_))
    }
    /// Append a recursive local function declaration.
    pub fn localfunc(&mut self, name: &str, fn_: NodeId) -> NodeId {
        let s = self.sym(name);
        self.push(Node::Localfunc(s, fn_))
    }
    /// Append local declarations with value-adjusted initializers.
    pub fn local(&mut self, names: Vec<String>, es: Vec<NodeId>) -> NodeId {
        let s: Vec<SymbolId> = names.iter().map(|n| self.sym(n)).collect();
        self.push(Node::Local(s, es))
    }
    /// Append a return statement preserving its expression list.
    pub fn ret(&mut self, es: Vec<NodeId>) -> NodeId {
        self.push(Node::Return(es))
    }
    /// Append a call used as a statement.
    pub fn callstat(&mut self, e: NodeId) -> NodeId {
        self.push(Node::Callstat(e))
    }
    /// Append assignments preserving both expression-list orders.
    pub fn assign(&mut self, vs: Vec<NodeId>, es: Vec<NodeId>) -> NodeId {
        self.push(Node::Assign(vs, es))
    }
    /// Append a nil expression.
    pub fn nil(&mut self) -> NodeId {
        self.push(Node::Nil)
    }
    /// Append a boolean expression.
    pub fn bool_(&mut self, v: bool) -> NodeId {
        self.push(Node::Bool(v))
    }
    /// Append a variadic argument expression.
    pub fn vararg(&mut self) -> NodeId {
        self.push(Node::Vararg)
    }
    /// Append a numeric literal, retaining its source spelling.
    pub fn num(&mut self, v: String) -> NodeId {
        self.push(Node::Num(v.into()))
    }
    /// Append an already-quoted Lua string literal.
    pub fn str(&mut self, v: String) -> NodeId {
        self.push(Node::Str(v.into()))
    }
    /// Append a function value with parameters and body.
    pub fn function(&mut self, ps: Vec<String>, variadic: bool, b: NodeId) -> NodeId {
        let s: Vec<SymbolId> = ps.iter().map(|n| self.sym(n)).collect();
        self.push(Node::Function(s, variadic, b))
    }
    /// Append an ordered table constructor.
    pub fn table(&mut self, fs: Vec<TableField>) -> NodeId {
        self.push(Node::Table(fs))
    }
    /// Append a unary expression.
    pub fn un(&mut self, op: &str, e: NodeId) -> NodeId {
        self.push(Node::Un(op.to_string(), e))
    }
    /// Append a binary expression.
    pub fn bin(&mut self, op: &str, l: NodeId, r: NodeId) -> NodeId {
        self.push(Node::Bin(op.to_string(), l, r))
    }
    /// Append parentheses, retaining Lua single-value adjustment.
    pub fn paren(&mut self, e: NodeId) -> NodeId {
        self.push(Node::Paren(e))
    }
    /// Append a function or method call with ordered arguments.
    pub fn call(&mut self, fn_: NodeId, args: Vec<NodeId>, method: Option<String>) -> NodeId {
        self.push(Node::Call(fn_, args, method))
    }
    /// Append an identifier reference, interning its name.
    pub fn name(&mut self, v: &str) -> NodeId {
        let s = self.sym(v);
        self.push(Node::Name(s))
    }
    /// Append an object/key access with the requested syntax flag.
    pub fn index(&mut self, obj: NodeId, key: NodeId, dot: bool) -> NodeId {
        self.push(Node::Index(obj, key, dot))
    }
    /// Append a method declaration target.
    pub fn methodname(&mut self, obj: NodeId, name: &str) -> NodeId {
        let s = self.sym(name);
        self.push(Node::Methodname(obj, s))
    }
}

impl Default for Ast {
    fn default() -> Self {
        Self::new()
    }
}

// Human-readable syntax snapshots retain the historical nodes/strings shape.
// Worker transfers use a sized tuple; serde flatten requires a map length that
// bincode cannot know and is therefore deliberately not used for binary data.
impl Serialize for Ast {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        if serializer.is_human_readable() {
            use serde::ser::SerializeStruct;
            let origins = self.nodes.provenance();
            let mut state =
                serializer.serialize_struct("Ast", if origins.is_some() { 3 } else { 2 })?;
            state.serialize_field("nodes", &*self.nodes)?;
            state.serialize_field("strings", &self.strings)?;
            if let Some(origins) = origins {
                state.serialize_field("origins", origins)?;
            }
            state.end()
        } else {
            (&self.nodes, &self.strings).serialize(serializer)
        }
    }
}
impl<'de> Deserialize<'de> for Ast {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        if deserializer.is_human_readable() {
            #[derive(Deserialize)]
            struct Wire {
                nodes: Vec<Node>,
                strings: Interner,
                origins: Option<crate::provenance::ArenaOrigins>,
            }
            let wire = Wire::deserialize(deserializer)?;
            let nodes = crate::node_arena::NodeArena::from_parts(wire.nodes, wire.origins)
                .map_err(serde::de::Error::custom)?;
            Ok(Self {
                nodes,
                strings: wire.strings,
            })
        } else {
            let (nodes, strings) = Deserialize::deserialize(deserializer)?;
            Ok(Self { nodes, strings })
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod interner_registry_tests {
    use super::Interner;

    #[test]
    fn copy_on_write_detaches_only_when_a_new_identifier_is_inserted() {
        use std::sync::Arc;
        let mut original = Interner::new();
        let first = original.intern("existing");
        let mut branch = original.clone();
        assert!(Arc::ptr_eq(&original.map, &branch.map));
        assert!(Arc::ptr_eq(&original.strings, &branch.strings));
        assert_eq!(branch.intern("existing"), first);
        assert!(Arc::ptr_eq(&original.map, &branch.map));
        assert!(Arc::ptr_eq(&original.strings, &branch.strings));
        branch.intern("branch_only");
        assert!(!Arc::ptr_eq(&original.map, &branch.map));
        assert!(!Arc::ptr_eq(&original.strings, &branch.strings));
        assert_eq!(original.get(first).as_ptr(), branch.get(first).as_ptr());
        original.intern("original_only");
        assert!(!original.contains("branch_only"));
        assert!(!branch.contains("original_only"));
        assert_eq!(original.iter().collect::<Vec<_>>(), original.all_strings());
        assert_eq!(branch.iter().collect::<Vec<_>>(), branch.all_strings());
    }

    #[test]
    fn borrowed_registry_queries_match_owned_snapshots_and_symbol_order() {
        let mut a = Interner::new();
        let mut b = Interner::new();
        let mut reordered = Interner::new();
        for name in ["alpha", "_ENV", "gamma"] {
            a.intern(name);
            b.intern(name);
        }
        for name in ["gamma", "_ENV", "alpha"] {
            reordered.intern(name);
        }
        for name in ["alpha", "_ENV", "gamma", "_ENV_suffix", "missing"] {
            assert_eq!(a.contains(name), a.all_strings().iter().any(|s| s == name));
        }
        assert!(a.same_symbols(&b));
        assert!(!a.same_symbols(&reordered));
        let mut branch = a.clone();
        branch.intern("new_symbol");
        assert!(!a.same_symbols(&branch));
        assert!(!a.contains("new_symbol"));
        assert!(a.same_symbols(&b));
    }
}
