//! Structured explanation records captured by the compiler.
//! These do not claim runtime variable locations or a proof of semantic equivalence.
use crate::{provenance::SourceSpan, Node};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// Semantic role of an additional original range.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RelationRole {
    /// Another input to the generated construct.
    Contribution,
    /// Definition of a copied/shared construct.
    Definition,
    /// Function invocation or inlining site.
    CallSite,
    /// Actual argument supplied by a caller.
    Argument,
    /// Read of a formal parameter in the original body.
    ParameterUse,
    /// Original use of a substituted value.
    UseSite,
}
/// A typed relationship into the same source snapshot table.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SourceRelation {
    /// Why the range is related.
    pub role: RelationRole,
    /// Original half-open UTF-8 range.
    pub span: SourceSpan,
}
/// An enclosing expansion retained independently of a leaf's own source position.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InlineContext {
    /// Original expression/body expanded here, not necessarily a declaration.
    pub definition: SourceSpan,
    /// Invocation that triggered the expansion.
    pub call_site: SourceSpan,
}
/// Observed action recorded at a mutation, not inferred from final generated text.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OptimizationReason {
    /// Stable internal transformation rule identifier.
    pub code: Arc<str>,
    /// The operation observed by the compiler.
    pub operation: ReasonOperation,
    /// Immediate input shape or literal spelling, when captured.
    pub before: Option<Arc<str>>,
    /// Immediate result shape or literal spelling, when captured.
    pub after: Option<Arc<str>>,
    /// Explicit eligibility/selection basis, when this rule records its checks.
    pub basis: Option<Arc<str>>,
    /// Actual checked values/settings at the decision, with explicitly named encodings.
    pub facts: Arc<Vec<ReasonFact>>,
}
/// A stable key and lossless textual value observed while accepting a rewrite.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReasonFact {
    /// Fact identifier; units/encoding are part of the key or value tag.
    pub key: Arc<str>,
    /// Observed value, not an expression to evaluate at display time.
    pub value: Arc<str>,
}

/// A fact value for `OptimizationReason::decision_shared`: either a freshly
/// formatted string, interned on first sight exactly as `decision` does, or
/// a caller-shared `Arc<str>` reused across reasons without per-node
/// allocation. The two variants exist because converting a formatted string
/// into an `Arc<str>` would allocate a second buffer per node; shared
/// values must arrive already shared for the saving to hold. A borrowed
/// value is interned exactly like an owned one, but the caller need not
/// allocate a `String` for it first: on a cache hit nothing is allocated,
/// and on a miss only the single interned `Arc<str>` is allocated.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum DecisionFactValue<'a> {
    /// A freshly formatted value, interned on first sight.
    Owned(String),
    /// A caller-owned value shared across reasons (no per-node allocation).
    Shared(Arc<str>),
    /// A borrowed value, interned on first sight without a caller `String`.
    Borrowed(&'a str),
}

/// Explanation actions include more than strictly size-reducing operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ReasonOperation {
    /// Structural rewrite after the transformation's checks succeeded.
    Rewrite,
    /// Binding-preserving name allocation.
    Rename,
    /// Association with an explicitly identified source.
    Relate,
    /// Compiler-created storage/control code.
    Synthesize,
    /// A construct was detached from the candidate syntax tree.
    Remove,
}
/// An action on original syntax captured for one candidate.
/// Removal here does not imply that no derived or copied occurrence survives.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SourceDisposition {
    /// Affected original construct.
    pub original: SourceSpan,
    /// Rule and observed structural change.
    pub reason: OptimizationReason,
    /// Known source ranges of direct replacement constructs, not runtime PCs.
    pub replacement_sources: Vec<SourceSpan>,
}
/// One shared empty facts list for reasons that record no facts, so a
/// rewrite reason does not allocate a fresh empty box per node.
fn empty_facts() -> Arc<Vec<ReasonFact>> {
    static EMPTY: std::sync::OnceLock<Arc<Vec<ReasonFact>>> = std::sync::OnceLock::new();
    Arc::clone(EMPTY.get_or_init(Arc::default))
}

/// Shared spelling/value cache for decision reasons. A decision reason
/// repeats the same short spellings at every rewritten node (the same rule,
/// basis, and fact names), so the first spelling is retained and later
/// reasons share it. Only equal strings are shared, so serialized
/// explanations are unchanged. Fact values are short structured
/// observations (operators, literal tags, sizes, tolerance bits) that also
/// repeat across nodes, so equal values share one `Arc<str>` too. Unlike
/// spellings, values can be distinct per node, so their cache is capped:
/// once it is full, further values fall back to a fresh `Arc<str>` (the
/// previous behaviour) instead of growing with input size, and the
/// process-wide retained set stays bounded. Both `decision` and
/// `decision_shared` use this cache, so a value shared by one entry point
/// is shared by the other as well.
struct DecisionIntern {
    spellings: std::collections::HashMap<Box<str>, Arc<str>>,
    fact_values: std::collections::HashMap<Box<str>, Arc<str>>,
    fact_sets: std::collections::HashMap<FactSetKey, Arc<Vec<ReasonFact>>>,
}

const MAX_FACT_VALUE_ENTRIES: usize = 4096;

/// Cap for shared decision fact lists, matching the fact-value cap. Once
/// full, further lists fall back to a fresh allocation (the previous
/// behaviour) instead of growing with input size.
const MAX_FACT_SET_ENTRIES: usize = 4096;

/// Most decision fact lists a caller passes to `decision_shared` (the
/// folding evidence lists have 7 and 10 entries). Longer lists bypass the
/// fact-list cache and keep the previous behaviour.
const MAX_DECISION_FACTS: usize = 10;

/// Cache key for a decision reason's fact list: the interned key/value
/// spellings in order. Arcs compare by content, and the key holds strong
/// references, so a cached fact list can never be confused with a later
/// string that merely reuses a freed allocation. The array lives on the
/// stack, so a cache hit allocates nothing.
#[derive(PartialEq, Eq, Hash)]
struct FactSetKey {
    len: usize,
    pairs: [Option<(Arc<str>, Arc<str>)>; MAX_DECISION_FACTS],
}

static DECISION_INTERN: std::sync::OnceLock<std::sync::Mutex<DecisionIntern>> =
    std::sync::OnceLock::new();

fn decision_intern() -> std::sync::MutexGuard<'static, DecisionIntern> {
    DECISION_INTERN
        .get_or_init(|| {
            std::sync::Mutex::new(DecisionIntern {
                spellings: std::collections::HashMap::new(),
                fact_values: std::collections::HashMap::new(),
                fact_sets: std::collections::HashMap::new(),
            })
        })
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

impl OptimizationReason {
    /// Reject malformed explanation records even when the outer checksum is valid.
    pub fn validate(&self) -> Result<(), String> {
        if self.code.is_empty() || self.basis.as_ref().is_some_and(|b| b.is_empty()) {
            return Err("optimization reason has an empty code or basis".into());
        }
        let mut keys = std::collections::HashSet::new();
        for fact in self.facts.iter() {
            if fact.key.is_empty() || !keys.insert(fact.key.as_ref()) {
                return Err("optimization reason has an empty or duplicate fact key".into());
            }
        }
        Ok(())
    }

    /// Record successful checks where the optimizer actually makes its decision.
    /// Missing facts remain absent rather than inferred from a final pass name.
    pub fn decision(
        code: &str,
        basis: &str,
        facts: impl IntoIterator<Item = (&'static str, String)>,
    ) -> Self {
        let mut interned = decision_intern();
        let intern = |value: &str, cache: &mut std::collections::HashMap<Box<str>, Arc<str>>| {
            if let Some(shared) = cache.get(value) {
                return Arc::clone(shared);
            }
            let shared: Arc<str> = value.into();
            cache.insert(value.into(), Arc::clone(&shared));
            shared
        };
        Self {
            code: intern(code, &mut interned.spellings),
            operation: ReasonOperation::Rewrite,
            before: None,
            after: None,
            basis: Some(intern(basis, &mut interned.spellings)),
            facts: Arc::new(
                facts
                    .into_iter()
                    .map(|(key, value)| {
                        let key = intern(key, &mut interned.spellings);
                        let value = if let Some(shared) = interned.fact_values.get(value.as_str()) {
                            Arc::clone(shared)
                        } else if interned.fact_values.len() < MAX_FACT_VALUE_ENTRIES {
                            let shared: Arc<str> = value.into();
                            interned
                                .fact_values
                                .insert(shared.as_ref().into(), Arc::clone(&shared));
                            shared
                        } else {
                            value.into()
                        };
                        ReasonFact { key, value }
                    })
                    .collect(),
            ),
        }
    }

    /// Same as `decision`, but each fact value is supplied either as a
    /// freshly formatted string (handled exactly as `decision` handles it)
    /// or as a caller-shared `Arc<str>`. A caller that folds many nodes
    /// under one rule (for example constant-folding, where the tolerance
    /// bits and the aggressive flag are identical for every node) builds
    /// those shared values once and passes the same `Arc<str>` to every
    /// reason, so no per-node string allocation is paid for them. Passing
    /// a shared value must not go through a `String` first: converting a
    /// formatted string into an `Arc<str>` would allocate a second buffer
    /// per node and erase the saving, so shared values keep their own
    /// variant. The recorded reason is identical to the one `decision`
    /// would record for the same facts: values still pass through the
    /// shared value cache, so equal values share one allocation either
    /// way, and only the caller-side formatting allocation is avoided.
    /// Identical fact lists share one facts allocation as well (the list
    /// is cached by content, capped like the value cache), so a repeated
    /// fold of the same literals pays no per-node facts allocation.
    pub fn decision_shared<'a>(
        code: &str,
        basis: &str,
        facts: impl IntoIterator<Item = (&'static str, DecisionFactValue<'a>)>,
    ) -> Self {
        let mut interned = decision_intern();
        let intern = |value: &str, cache: &mut std::collections::HashMap<Box<str>, Arc<str>>| {
            if let Some(shared) = cache.get(value) {
                return Arc::clone(shared);
            }
            let shared: Arc<str> = value.into();
            cache.insert(value.into(), Arc::clone(&shared));
            shared
        };
        let code = intern(code, &mut interned.spellings);
        let basis = Some(intern(basis, &mut interned.spellings));
        // Materialize the fact list on the stack first. Identical fact
        // lists (the same rule applied to the same literals, which repeats
        // across a fold) then share one facts allocation instead of paying
        // a fresh Vec plus Arc box per node. The recorded reason is
        // identical either way: keys and values compare by content.
        let mut pairs: [Option<(Arc<str>, Arc<str>)>; MAX_DECISION_FACTS] =
            [const { None }; MAX_DECISION_FACTS];
        let mut len = 0usize;
        let mut tail: Vec<(Arc<str>, Arc<str>)> = Vec::new();
        for (key, value) in facts {
            let key = intern(key, &mut interned.spellings);
            let value = match value {
                DecisionFactValue::Owned(value) => {
                    if let Some(shared) = interned.fact_values.get(value.as_str()) {
                        Arc::clone(shared)
                    } else if interned.fact_values.len() < MAX_FACT_VALUE_ENTRIES {
                        let shared: Arc<str> = value.into();
                        interned
                            .fact_values
                            .insert(shared.as_ref().into(), Arc::clone(&shared));
                        shared
                    } else {
                        value.into()
                    }
                }
                DecisionFactValue::Shared(value) => {
                    if let Some(shared) = interned.fact_values.get(value.as_ref()) {
                        Arc::clone(shared)
                    } else if interned.fact_values.len() < MAX_FACT_VALUE_ENTRIES {
                        interned
                            .fact_values
                            .insert(value.as_ref().into(), Arc::clone(&value));
                        value
                    } else {
                        value
                    }
                }
                DecisionFactValue::Borrowed(value) => {
                    if let Some(shared) = interned.fact_values.get(value) {
                        Arc::clone(shared)
                    } else if interned.fact_values.len() < MAX_FACT_VALUE_ENTRIES {
                        let shared: Arc<str> = value.into();
                        interned
                            .fact_values
                            .insert(shared.as_ref().into(), Arc::clone(&shared));
                        shared
                    } else {
                        value.into()
                    }
                }
            };
            if len < MAX_DECISION_FACTS {
                pairs[len] = Some((key, value));
                len += 1;
            } else {
                tail.push((key, value));
            }
        }
        let facts = if tail.is_empty() {
            if len == 0 {
                empty_facts()
            } else {
                let key = FactSetKey {
                    len,
                    pairs: pairs.clone(),
                };
                if let Some(shared) = interned.fact_sets.get(&key) {
                    Arc::clone(shared)
                } else {
                    let built: Arc<Vec<ReasonFact>> = Arc::new(
                        pairs[..len]
                            .iter()
                            .flatten()
                            .map(|(key, value)| ReasonFact {
                                key: Arc::clone(key),
                                value: Arc::clone(value),
                            })
                            .collect(),
                    );
                    if interned.fact_sets.len() < MAX_FACT_SET_ENTRIES {
                        interned.fact_sets.insert(key, Arc::clone(&built));
                    }
                    built
                }
            }
        } else {
            // Longer than the cached shape: keep the previous behaviour.
            let mut all: Vec<ReasonFact> = pairs[..len]
                .iter()
                .flatten()
                .map(|(key, value)| ReasonFact {
                    key: Arc::clone(key),
                    value: Arc::clone(value),
                })
                .collect();
            all.extend(
                tail.into_iter()
                    .map(|(key, value)| ReasonFact { key, value }),
            );
            Arc::new(all)
        };
        Self {
            code,
            operation: ReasonOperation::Rewrite,
            before: None,
            after: None,
            basis,
            facts,
        }
    }
    pub(crate) fn action(code: &str, operation: ReasonOperation) -> Self {
        Self::action_shared(code.into(), operation)
    }
    pub(crate) fn action_shared(code: Arc<str>, operation: ReasonOperation) -> Self {
        Self {
            code,
            operation,
            before: None,
            after: None,
            basis: None,
            facts: empty_facts(),
        }
    }
    /// Record a shape-changing rewrite. The code string is supplied by the
    /// caller so a batch rewrite shares one allocation; the stored value is
    /// compared by string content, so the recorded reason is identical
    /// regardless of which allocation supplied it.
    pub(crate) fn rewrite_shared(code: &Arc<str>, before: &Node, after: &Node) -> Self {
        Self {
            code: Arc::clone(code),
            operation: ReasonOperation::Rewrite,
            before: Some(shape(before)),
            after: Some(shape(after)),
            basis: None,
            facts: empty_facts(),
        }
    }
}
/// Operator shapes shared across rewrite reasons. Lua's operator set is
/// closed, so every binary/unary shape is one table entry instead of a
/// fresh format per node. The lookup is keyed by the operator itself, so
/// it allocates nothing. Unknown spellings (not valid Lua operators) fall
/// back to formatting, preserving the recorded string exactly.
fn operator_shape(binary: bool, op: &str) -> Arc<str> {
    static BIN_SHAPES: std::sync::OnceLock<std::collections::HashMap<&'static str, Arc<str>>> =
        std::sync::OnceLock::new();
    static UN_SHAPES: std::sync::OnceLock<std::collections::HashMap<&'static str, Arc<str>>> =
        std::sync::OnceLock::new();
    let (table, prefix) = if binary {
        (
            BIN_SHAPES.get_or_init(|| {
                [
                    ("+", "binary:+"),
                    ("-", "binary:-"),
                    ("*", "binary:*"),
                    ("/", "binary:/"),
                    ("%", "binary:%"),
                    ("^", "binary:^"),
                    ("..", "binary:.."),
                    ("==", "binary:=="),
                    ("~=", "binary:~="),
                    ("<=", "binary:<="),
                    (">=", "binary:>="),
                    ("<", "binary:<"),
                    (">", "binary:>"),
                    ("and", "binary:and"),
                    ("or", "binary:or"),
                ]
                .iter()
                .map(|(op, shape)| (*op, Arc::from(*shape)))
                .collect()
            }),
            "binary",
        )
    } else {
        (
            UN_SHAPES.get_or_init(|| {
                [("-", "unary:-"), ("not", "unary:not"), ("#", "unary:#")]
                    .iter()
                    .map(|(op, shape)| (*op, Arc::from(*shape)))
                    .collect()
            }),
            "unary",
        )
    };
    match table.get(op) {
        Some(shared) => Arc::clone(shared),
        None => format!("{prefix}:{op}").into(),
    }
}

/// Shared value-dependent node shapes for rewrite reasons. A numeric shape
/// is keyed by the literal spelling itself and a string shape by byte length,
/// so a repeated shape is found without formatting anything first: a hit
/// allocates nothing, while a miss formats exactly once like before. The
/// cache is capped like the decision intern cache; beyond the cap, shapes
/// fall back to fresh allocations with identical content.
struct ShapeIntern {
    nums: std::collections::HashMap<Box<str>, Arc<str>>,
    strs: std::collections::HashMap<usize, Arc<str>>,
}

const MAX_SHAPE_ENTRIES: usize = 4096;

fn shape_intern() -> std::sync::MutexGuard<'static, ShapeIntern> {
    static SHAPE_INTERN: std::sync::OnceLock<std::sync::Mutex<ShapeIntern>> =
        std::sync::OnceLock::new();
    SHAPE_INTERN
        .get_or_init(|| {
            std::sync::Mutex::new(ShapeIntern {
                nums: std::collections::HashMap::new(),
                strs: std::collections::HashMap::new(),
            })
        })
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn num_shape(spelling: &str) -> Arc<str> {
    let mut interned = shape_intern();
    if let Some(shared) = interned.nums.get(spelling) {
        return Arc::clone(shared);
    }
    let shared: Arc<str> = format!("number:{spelling}").into();
    if interned.nums.len() < MAX_SHAPE_ENTRIES {
        interned.nums.insert(spelling.into(), Arc::clone(&shared));
    }
    shared
}

fn str_shape(byte_len: usize) -> Arc<str> {
    let mut interned = shape_intern();
    if let Some(shared) = interned.strs.get(&byte_len) {
        return Arc::clone(shared);
    }
    let shared: Arc<str> = format!("string-literal-bytes:{byte_len}").into();
    if interned.strs.len() < MAX_SHAPE_ENTRIES {
        interned.strs.insert(byte_len, Arc::clone(&shared));
    }
    shared
}

/// Fixed node-shape spellings shared across rewrite reasons, so a wide
/// rewrite does not allocate one copy of "local"/"assignment"/... per node.
/// The table is built once and only holds the closed set of fixed spellings;
/// value-dependent shapes (numbers, strings) use the shared shape cache.
fn fixed_shape(spelling: &'static str) -> Arc<str> {
    static SHAPES: std::sync::OnceLock<std::collections::HashMap<&'static str, Arc<str>>> =
        std::sync::OnceLock::new();
    let table = SHAPES.get_or_init(|| {
        [
            "nil",
            "boolean:true",
            "boolean:false",
            "name",
            "vararg",
            "parenthesized",
            "table",
            "index",
            "function",
            "call",
            "method",
            "block",
            "local",
            "local-function",
            "assignment",
            "return",
            "call-statement",
            "function-statement",
            "if",
            "while",
            "repeat",
            "numeric-for",
            "generic-for",
            "do",
            "break",
            "goto",
            "label",
        ]
        .iter()
        .map(|spelling| (*spelling, Arc::from(*spelling)))
        .collect()
    });
    Arc::clone(&table[spelling])
}
fn shape(node: &Node) -> Arc<str> {
    // Numeric values stay literal strings, preserving i64 digits and nonfinite values.
    match node {
        Node::Num(v) => num_shape(v),
        Node::Str(v) => str_shape(v.len()),
        Node::Bool(v) => fixed_shape(if *v { "boolean:true" } else { "boolean:false" }),
        Node::Bin(op, _, _) => operator_shape(true, op),
        Node::Un(op, _) => operator_shape(false, op),
        Node::Nil => fixed_shape("nil"),
        Node::Name(_) => fixed_shape("name"),
        Node::Vararg => fixed_shape("vararg"),
        Node::Paren(_) => fixed_shape("parenthesized"),
        Node::Table(_) => fixed_shape("table"),
        Node::Index(..) => fixed_shape("index"),
        Node::Function(..) => fixed_shape("function"),
        Node::Call(..) => fixed_shape("call"),
        Node::Methodname(..) => fixed_shape("method"),
        Node::Block(_) => fixed_shape("block"),
        Node::Local(..) => fixed_shape("local"),
        Node::Localfunc(..) => fixed_shape("local-function"),
        Node::Assign(..) => fixed_shape("assignment"),
        Node::Return(_) => fixed_shape("return"),
        Node::Callstat(_) => fixed_shape("call-statement"),
        Node::Funcstat(..) => fixed_shape("function-statement"),
        Node::If(..) => fixed_shape("if"),
        Node::While(..) => fixed_shape("while"),
        Node::Repeat(..) => fixed_shape("repeat"),
        Node::Fornum(..) => fixed_shape("numeric-for"),
        Node::Forin(..) => fixed_shape("generic-for"),
        Node::Do(_) => fixed_shape("do"),
        Node::Break => fixed_shape("break"),
        Node::Goto(_) => fixed_shape("goto"),
        Node::Label(_) => fixed_shape("label"),
    }
}

/// Source token with an explicit syntactic role, independent of child expressions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SyntaxSite {
    /// Operator owned by one unary or binary expression.
    Operator,
    /// First keyword of a statement or function expression.
    Begin,
    /// Keyword between a controlling expression and its body (or repeat condition).
    Body,
    /// Closing keyword of a compound statement or function.
    End,
}
pub(crate) fn syntax_text(node: &Node, site: SyntaxSite) -> Option<&str> {
    match (node, site) {
        (Node::Bin(op, ..) | Node::Un(op, _), SyntaxSite::Operator) => Some(op),
        (Node::Function(..) | Node::Funcstat(..), SyntaxSite::Begin) => Some("function"),
        (Node::Local(..) | Node::Localfunc(..), SyntaxSite::Begin) => Some("local"),
        (Node::If(..), SyntaxSite::Begin) => Some("if"),
        (Node::While(..), SyntaxSite::Begin) => Some("while"),
        (Node::Repeat(..), SyntaxSite::Begin) => Some("repeat"),
        (Node::Fornum(..) | Node::Forin(..), SyntaxSite::Begin) => Some("for"),
        (Node::Do(_), SyntaxSite::Begin) => Some("do"),
        (Node::Return(_), SyntaxSite::Begin) => Some("return"),
        (Node::Break, SyntaxSite::Begin) => Some("break"),
        (Node::If(..), SyntaxSite::Body) => Some("then"),
        (Node::While(..) | Node::Fornum(..) | Node::Forin(..), SyntaxSite::Body) => Some("do"),
        (Node::Repeat(..), SyntaxSite::Body) => Some("until"),
        (Node::Goto(_), SyntaxSite::Begin) => Some("goto"),
        (
            Node::Function(..)
            | Node::If(..)
            | Node::While(..)
            | Node::Fornum(..)
            | Node::Forin(..)
            | Node::Do(_)
            | Node::Funcstat(..)
            | Node::Localfunc(..),
            SyntaxSite::End,
        ) => Some("end"),
        _ => None,
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn operator_and_boolean_shapes_are_shared_and_exact() {
        // Repeated shapes must resolve to the same allocation, and the
        // recorded strings must stay exactly the formatted forms.
        let plus = shape(&Node::Bin("+".to_string(), 0, 0));
        assert_eq!(plus.as_ref(), "binary:+");
        assert!(Arc::ptr_eq(
            &plus,
            &shape(&Node::Bin("+".to_string(), 0, 0))
        ));
        let not = shape(&Node::Un("not".to_string(), 0));
        assert_eq!(not.as_ref(), "unary:not");
        assert!(Arc::ptr_eq(&not, &shape(&Node::Un("not".to_string(), 0))));
        let truth = shape(&Node::Bool(true));
        assert_eq!(truth.as_ref(), "boolean:true");
        assert!(Arc::ptr_eq(&truth, &shape(&Node::Bool(true))));
        assert_eq!(shape(&Node::Bool(false)).as_ref(), "boolean:false");
        // Unknown spellings keep formatting, never the table entry.
        assert_eq!(
            shape(&Node::Bin("???".to_string(), 0, 0)).as_ref(),
            "binary:???"
        );
    }

    #[test]
    fn numeric_and_string_shapes_are_shared_and_exact() {
        // Value-dependent shapes must resolve to the same allocation on
        // repeat, with exactly the formatted content: numbers by spelling,
        // strings by byte length alone.
        let three = shape(&Node::Num("3".into()));
        assert_eq!(three.as_ref(), "number:3");
        assert!(Arc::ptr_eq(&three, &shape(&Node::Num("3".into()))));
        let other = shape(&Node::Num("3.5".into()));
        assert_eq!(other.as_ref(), "number:3.5");
        assert!(!Arc::ptr_eq(&three, &other));
        let short = shape(&Node::Str("ab".into()));
        assert_eq!(short.as_ref(), "string-literal-bytes:2");
        assert!(Arc::ptr_eq(&short, &shape(&Node::Str("xy".into()))));
        assert!(!Arc::ptr_eq(&short, &shape(&Node::Str("xyz".into()))));
    }

    #[test]
    fn decision_shared_matches_decision() {
        // The same facts recorded through either entry point must compare
        // equal, and a caller-owned shared value must be reused as-is when
        // the cache has no entry yet (checked via pointer equality on a
        // fresh value spelling).
        let owned = OptimizationReason::decision(
            "constant-folding",
            "literal-unary-evaluation",
            [
                ("operator", "-".to_string()),
                ("operand", "i64:7".to_string()),
                ("aggressive", "true".to_string()),
            ],
        );
        let shared_value: Arc<str> = Arc::from("i64:7");
        let shared = OptimizationReason::decision_shared(
            "constant-folding",
            "literal-unary-evaluation",
            [
                ("operator", DecisionFactValue::Owned("-".to_string())),
                (
                    "operand",
                    DecisionFactValue::Shared(Arc::clone(&shared_value)),
                ),
                ("aggressive", DecisionFactValue::Shared(Arc::from("true"))),
            ],
        );
        assert_eq!(owned, shared);
        assert_eq!(owned.facts.len(), 3);
        assert_eq!(shared.facts[1].value.as_ref(), "i64:7");
        // A borrowed value records exactly the same reason as the owned
        // spelling, without the caller allocating a `String` for it.
        let borrowed = OptimizationReason::decision_shared(
            "constant-folding",
            "literal-unary-evaluation",
            [
                ("operator", DecisionFactValue::Borrowed("-")),
                ("operand", DecisionFactValue::Borrowed("i64:7")),
                ("aggressive", DecisionFactValue::Shared(Arc::from("true"))),
            ],
        );
        assert_eq!(owned, borrowed);
    }

    #[test]
    fn decision_shared_reuses_identical_fact_lists() {
        // Identical fact lists share one facts allocation; a list that
        // differs in one value gets its own. Content equality is what the
        // recorded reason guarantees, pointer equality is the sharing.
        let first = OptimizationReason::decision_shared(
            "test-fact-list-sharing",
            "test-basis",
            [
                ("operator", DecisionFactValue::Borrowed("+")),
                ("operand", DecisionFactValue::Borrowed("i64:12345")),
                ("aggressive", DecisionFactValue::Shared(Arc::from("true"))),
            ],
        );
        let second = OptimizationReason::decision_shared(
            "test-fact-list-sharing",
            "test-basis",
            [
                ("operator", DecisionFactValue::Borrowed("+")),
                ("operand", DecisionFactValue::Borrowed("i64:12345")),
                ("aggressive", DecisionFactValue::Shared(Arc::from("true"))),
            ],
        );
        assert_eq!(first, second);
        assert!(Arc::ptr_eq(&first.facts, &second.facts));
        let different = OptimizationReason::decision_shared(
            "test-fact-list-sharing",
            "test-basis",
            [
                ("operator", DecisionFactValue::Borrowed("+")),
                ("operand", DecisionFactValue::Borrowed("i64:54321")),
                ("aggressive", DecisionFactValue::Shared(Arc::from("true"))),
            ],
        );
        assert_ne!(first, different);
        assert!(!Arc::ptr_eq(&first.facts, &different.facts));
    }
}
