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
        Self {
            code: code.into(),
            operation: ReasonOperation::Rewrite,
            before: None,
            after: None,
            basis: Some(basis.into()),
            facts: Arc::new(
                facts
                    .into_iter()
                    .map(|(key, value)| ReasonFact {
                        key: key.into(),
                        value: value.into(),
                    })
                    .collect(),
            ),
        }
    }
    pub(crate) fn action(code: &str, operation: ReasonOperation) -> Self {
        Self {
            code: code.into(),
            operation,
            before: None,
            after: None,
            basis: None,
            facts: Arc::default(),
        }
    }
    pub(crate) fn rewrite(code: &str, before: &Node, after: &Node) -> Self {
        Self {
            code: code.into(),
            operation: ReasonOperation::Rewrite,
            before: Some(shape(before)),
            after: Some(shape(after)),
            basis: None,
            facts: Arc::default(),
        }
    }
}
fn shape(node: &Node) -> Arc<str> {
    // Numeric values stay literal strings, preserving i64 digits and nonfinite values.
    match node {
        Node::Num(v) => format!("number:{v}").into(),
        Node::Str(v) => format!("string-literal-bytes:{}", v.len()).into(),
        Node::Bool(v) => format!("boolean:{v}").into(),
        Node::Bin(op, _, _) => format!("binary:{op}").into(),
        Node::Un(op, _) => format!("unary:{op}").into(),
        Node::Nil => "nil".into(),
        Node::Name(_) => "name".into(),
        Node::Vararg => "vararg".into(),
        Node::Paren(_) => "parenthesized".into(),
        Node::Table(_) => "table".into(),
        Node::Index(..) => "index".into(),
        Node::Function(..) => "function".into(),
        Node::Call(..) => "call".into(),
        Node::Methodname(..) => "method".into(),
        Node::Block(_) => "block".into(),
        Node::Local(..) => "local".into(),
        Node::Localfunc(..) => "local-function".into(),
        Node::Assign(..) => "assignment".into(),
        Node::Return(_) => "return".into(),
        Node::Callstat(_) => "call-statement".into(),
        Node::Funcstat(..) => "function-statement".into(),
        Node::If(..) => "if".into(),
        Node::While(..) => "while".into(),
        Node::Repeat(..) => "repeat".into(),
        Node::Fornum(..) => "numeric-for".into(),
        Node::Forin(..) => "generic-for".into(),
        Node::Do(_) => "do".into(),
        Node::Break => "break".into(),
        Node::Goto(_) => "goto".into(),
        Node::Label(_) => "label".into(),
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
