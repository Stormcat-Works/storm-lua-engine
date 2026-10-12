//! require 検出と制限チェック（設計 §3.1）。
//!
//! モジュールチャンクの**トップレベル直下の文**として書かれた次の2形式のみを認識する:
//!
//! - `local NAME = require("KEY")`
//! - `require("KEY")`
//!
//! それ以外の require 使用はすべて診断化する（式の一部 → `require-not-statement`、
//! 関数内/ブロック内 → `require-not-top-level`、引数が文字列リテラル以外 → `require-dynamic`）。
//!
//! ここではキーの存在確認・循環検出（DFS）は行わない（P1b）。

use crate::diagnostic::{codes, Diagnostic, Range};
use storm_lua_syntax::ast::{Ast, Node, NodeId};
use storm_lua_syntax::ast_utils::for_each_child_key;
use storm_lua_syntax::numeric::decode_lua_string;
use storm_lua_syntax::parser::NodePositions;

/// 有効な require 検出結果（後続 P1b の DFS・静的展開が利用する）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequireSite {
    /// require 呼び出し文字列引数を復号したモジュールキー（照合は完全一致。§2.1）。
    pub key: String,
    /// `local NAME = require(...)` の場合の束縛名。`require(...)` 単独文の場合は None。
    pub binding: Option<String>,
    /// require 呼び出し式（Call ノード）自身の NodeId。
    pub node_id: NodeId,
    /// 診断・Source Map 逆引き用の位置。
    pub range: Option<Range>,
    /// この require を含むトップレベル文（`Local` または `Callstat`）自身の位置。
    /// リンク段（P1b）が静的展開時にソーステキストを文単位でスプライスするために使う
    /// （`range` は呼び出し式の位置であり、`local NAME = ` 部分を含まないため別に持つ）。
    pub stmt_range: Range,
}

struct ScanCtx<'a> {
    ast: &'a Ast,
    positions: &'a NodePositions,
    module: &'a str,
    sites: Vec<RequireSite>,
    diagnostics: Vec<Diagnostic>,
}

impl ScanCtx<'_> {
    fn range_of(&self, id: NodeId) -> Option<Range> {
        self.positions
            .get(id)
            .map(|(line, col)| Range::point(line, col))
    }

    fn push_violation(&mut self, code: &'static str, id: NodeId, message: impl Into<String>) {
        self.diagnostics.push(
            Diagnostic::error(code, message)
                .with_module(self.module.to_string())
                .with_range(self.range_of(id)),
        );
    }

    /// `call_id` が require 呼び出し文字列引数の Call ノードであることを前提に、
    /// 引数が単一の文字列リテラルかを検証し、site か `require-dynamic` 診断へ振り分ける。
    /// `stmt_id` はこの require を含むトップレベル文（`Local` または `Callstat`）自身の NodeId。
    fn record_require_call(&mut self, call_id: NodeId, binding: Option<String>, stmt_id: NodeId) {
        let Node::Call(_, args, _) = self.ast.node(call_id) else {
            unreachable!("record_require_call は Call ノードにのみ呼ばれる");
        };
        if args.len() == 1 {
            if let Node::Str(raw) = self.ast.node(args[0]) {
                let Some(key) = decode_lua_string(raw) else {
                    self.push_violation(
                        codes::REQUIRE_DYNAMIC,
                        call_id,
                        "require key must be UTF-8",
                    );
                    return;
                };
                #[expect(
                    clippy::expect_used,
                    reason = "The position table was produced with this AST and the parser marks every top-level statement"
                )]
                let stmt_range = self
                    .range_of(stmt_id)
                    .expect("トップレベル文は parser が常に位置をマークする");
                self.sites.push(RequireSite {
                    key,
                    binding,
                    node_id: call_id,
                    range: self.range_of(call_id),
                    stmt_range,
                });
                return;
            }
        }
        self.push_violation(
            codes::REQUIRE_DYNAMIC,
            call_id,
            "require() argument must be a string literal.",
        );
    }
}

/// `f` が require 呼び出しの対象（`Node::Name("require")`）かどうか。
fn is_require_target(ast: &Ast, f: NodeId) -> bool {
    matches!(ast.node(f), Node::Name(sym) if ast.strings.get(*sym) == "require")
}

/// require 呼び出し（`Call` かつ method なし かつ関数名が `require`）かどうか。
fn is_require_call(ast: &Ast, id: NodeId) -> bool {
    matches!(ast.node(id), Node::Call(f, _, method) if method.is_none() && is_require_target(ast, *f))
}

/// トップレベル外（関数内・if/for/while/do ブロック内）の式木を走査し、
/// require 呼び出しをすべて `require-not-top-level` として記録する。
/// `Function` に出会ったら本体ブロックを `visit_block(is_top_level=false)` として処理する。
fn scan_expr(ctx: &mut ScanCtx, id: NodeId, is_top_level: bool) {
    if let Node::Function(_, _, body) = ctx.ast.node(id) {
        let body = *body;
        visit_block(ctx, body, false);
        return;
    }
    if is_require_call(ctx.ast, id) {
        let code = if is_top_level {
            codes::REQUIRE_NOT_STATEMENT
        } else {
            codes::REQUIRE_NOT_TOP_LEVEL
        };
        let message = if is_top_level {
            "require() must be its own statement (`require(\"key\")` or `local x = require(\"key\")`)."
        } else {
            "require() is only recognized as a top-level statement of the module chunk."
        };
        ctx.push_violation(code, id, message);
    }
    let mut children = Vec::new();
    for_each_child_key(ctx.ast, id, &mut |_key, child| children.push(child));
    for child in children {
        scan_expr(ctx, child, is_top_level);
    }
}

/// ブロック直下の文を1つずつ判定する。`is_top_level` はこのブロックが
/// モジュールチャンクのルートブロックそのものかどうか（ネストしたら常に false）。
fn visit_block(ctx: &mut ScanCtx, block_id: NodeId, is_top_level: bool) {
    let Node::Block(stmts) = ctx.ast.node(block_id) else {
        unreachable!("visit_block は Block ノードにのみ呼ばれる");
    };
    let stmts = stmts.clone();
    for stmt_id in stmts {
        match ctx.ast.node(stmt_id) {
            Node::Callstat(e) => {
                let e = *e;
                if is_require_call(ctx.ast, e) {
                    if is_top_level {
                        ctx.record_require_call(e, None, stmt_id);
                    } else {
                        ctx.push_violation(
                            codes::REQUIRE_NOT_TOP_LEVEL,
                            e,
                            "require() is only recognized as a top-level statement of the module chunk.",
                        );
                    }
                } else {
                    scan_expr(ctx, e, is_top_level);
                }
            }
            Node::Local(names, es) => {
                let names = names.clone();
                let es = es.clone();
                if names.len() == 1 && es.len() == 1 && is_require_call(ctx.ast, es[0]) {
                    let binding = ctx.ast.strings.get(names[0]).to_string();
                    if is_top_level {
                        ctx.record_require_call(es[0], Some(binding), stmt_id);
                    } else {
                        ctx.push_violation(
                            codes::REQUIRE_NOT_TOP_LEVEL,
                            es[0],
                            "require() is only recognized as a top-level statement of the module chunk.",
                        );
                    }
                } else {
                    for e in es {
                        scan_expr(ctx, e, is_top_level);
                    }
                }
            }
            Node::Do(b) => {
                let b = *b;
                visit_block(ctx, b, false);
            }
            Node::While(e, b) => {
                let (e, b) = (*e, *b);
                scan_expr(ctx, e, is_top_level);
                visit_block(ctx, b, false);
            }
            Node::Repeat(b, e) => {
                let (b, e) = (*b, *e);
                visit_block(ctx, b, false);
                scan_expr(ctx, e, is_top_level);
            }
            Node::If(arms, eb) => {
                let arms = arms.clone();
                let eb = *eb;
                for arm in arms {
                    scan_expr(ctx, arm.cond, is_top_level);
                    visit_block(ctx, arm.body, false);
                }
                if let Some(eb) = eb {
                    visit_block(ctx, eb, false);
                }
            }
            Node::Fornum(_, a, b, c, body) => {
                let (a, b, c, body) = (*a, *b, *c, *body);
                scan_expr(ctx, a, is_top_level);
                scan_expr(ctx, b, is_top_level);
                if let Some(c) = c {
                    scan_expr(ctx, c, is_top_level);
                }
                visit_block(ctx, body, false);
            }
            Node::Forin(_, es, body) => {
                let es = es.clone();
                let body = *body;
                for e in es {
                    scan_expr(ctx, e, is_top_level);
                }
                visit_block(ctx, body, false);
            }
            Node::Funcstat(target, fn_) => {
                let (target, fn_) = (*target, *fn_);
                scan_expr(ctx, target, is_top_level);
                scan_expr(ctx, fn_, is_top_level);
            }
            Node::Localfunc(_, fn_) => {
                let fn_ = *fn_;
                scan_expr(ctx, fn_, is_top_level);
            }
            Node::Return(es) => {
                let es = es.clone();
                for e in es {
                    scan_expr(ctx, e, is_top_level);
                }
            }
            Node::Assign(vs, es) => {
                let vs = vs.clone();
                let es = es.clone();
                for v in vs {
                    scan_expr(ctx, v, is_top_level);
                }
                for e in es {
                    scan_expr(ctx, e, is_top_level);
                }
            }
            Node::Break | Node::Goto(_) | Node::Label(_) => {}
            _ => unreachable!("ブロック直下に現れない文種別"),
        }
    }
}

/// ambient モジュールソース内の require 呼び出しをすべて検出する（位置・文形式を問わない）。
/// ambient の source 内での require は形式に関わらず一律禁止（設計 §4.2）であり、
/// 通常モジュールの `scan_requires` のような2文形式チェックは行わない。
pub fn find_require_calls(ast: &Ast, root: NodeId) -> Vec<NodeId> {
    let mut found = Vec::new();
    storm_lua_syntax::ast_utils::walk(ast, root, &mut |id| {
        if is_require_call(ast, id) {
            found.push(id);
        }
    });
    found
}

/// モジュール `module` のパース済み AST から require 検出を行う。
/// 戻り値は (有効な require site 一覧, 制限違反の診断一覧)。
pub fn scan_requires(
    ast: &Ast,
    root: NodeId,
    positions: &NodePositions,
    module: &str,
) -> (Vec<RequireSite>, Vec<Diagnostic>) {
    let mut ctx = ScanCtx {
        ast,
        positions,
        module,
        sites: Vec::new(),
        diagnostics: Vec::new(),
    };
    visit_block(&mut ctx, root, true);
    (ctx.sites, ctx.diagnostics)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::diagnostic::Severity;
    use storm_lua_syntax::parser::parse_source_with_positions;

    fn scan(source: &str) -> (Vec<RequireSite>, Vec<Diagnostic>) {
        let (ast, root, positions) = parse_source_with_positions(source).expect("parse ok");
        scan_requires(&ast, root, &positions, "m")
    }

    fn codes_of(diagnostics: &[Diagnostic]) -> Vec<&'static str> {
        diagnostics.iter().map(|d| d.code).collect()
    }

    // --- 2つの正規形の検出 ---

    #[test]
    fn detects_binding_form_with_key_and_position() {
        // Call ノードの位置は開き括弧 `(` の位置（parser の mark 仕様どおり）。
        // "require(" の '(' は 1-based で列8。
        let (sites, diagnostics) = scan("require(\"lib.util\")\n");
        assert!(diagnostics.is_empty(), "{:?}", codes_of(&diagnostics));
        assert_eq!(sites.len(), 1);
        assert_eq!(sites[0].key, "lib.util");
        assert_eq!(sites[0].binding, None);
        assert_eq!(sites[0].range.map(|r| (r.line, r.col)), Some((1, 8)));
    }

    #[test]
    fn detects_binding_form_with_binding_name() {
        let (sites, diagnostics) = scan("local util = require(\"lib.util\")\n");
        assert!(diagnostics.is_empty(), "{:?}", codes_of(&diagnostics));
        assert_eq!(sites.len(), 1);
        assert_eq!(sites[0].key, "lib.util");
        assert_eq!(sites[0].binding.as_deref(), Some("util"));
    }

    #[test]
    fn detects_bare_statement_form_without_parens() {
        let (sites, diagnostics) = scan("require \"lib.side_effect\"\n");
        assert!(diagnostics.is_empty(), "{:?}", codes_of(&diagnostics));
        assert_eq!(sites.len(), 1);
        assert_eq!(sites[0].key, "lib.side_effect");
        assert_eq!(sites[0].binding, None);
    }

    #[test]
    fn detects_multiple_top_level_requires_in_order() {
        let (sites, diagnostics) =
            scan("local a = require(\"a\")\nrequire(\"b\")\nlocal c = require(\"c\")\n");
        assert!(diagnostics.is_empty());
        assert_eq!(
            sites.iter().map(|s| s.key.as_str()).collect::<Vec<_>>(),
            vec!["a", "b", "c"]
        );
    }

    // --- require-not-statement: 式の一部 ---

    #[test]
    fn flags_method_chain_after_require_as_not_statement() {
        let (sites, diagnostics) = scan("require(\"x\").f()\n");
        assert!(sites.is_empty());
        assert_eq!(codes_of(&diagnostics), vec![codes::REQUIRE_NOT_STATEMENT]);
    }

    #[test]
    fn flags_multi_value_local_as_not_statement() {
        let (sites, diagnostics) = scan("local a, b = require(\"x\"), 1\n");
        assert!(sites.is_empty());
        assert_eq!(codes_of(&diagnostics), vec![codes::REQUIRE_NOT_STATEMENT]);
    }

    #[test]
    fn flags_assignment_of_require_result_as_not_statement() {
        let (sites, diagnostics) = scan("a = require(\"x\")\n");
        assert!(sites.is_empty());
        assert_eq!(codes_of(&diagnostics), vec![codes::REQUIRE_NOT_STATEMENT]);
    }

    // --- require-not-top-level: 関数内・ブロック内 ---

    #[test]
    fn flags_require_inside_if_block_as_not_top_level() {
        let (sites, diagnostics) = scan("if true then\n  local a = require(\"x\")\nend\n");
        assert!(sites.is_empty());
        assert_eq!(codes_of(&diagnostics), vec![codes::REQUIRE_NOT_TOP_LEVEL]);
    }

    #[test]
    fn flags_require_inside_function_body_as_not_top_level() {
        let (sites, diagnostics) = scan("function f()\n  require(\"x\")\nend\n");
        assert!(sites.is_empty());
        assert_eq!(codes_of(&diagnostics), vec![codes::REQUIRE_NOT_TOP_LEVEL]);
    }

    #[test]
    fn flags_require_inside_do_block_as_not_top_level() {
        let (sites, diagnostics) = scan("do\n  require(\"x\")\nend\n");
        assert!(sites.is_empty());
        assert_eq!(codes_of(&diagnostics), vec![codes::REQUIRE_NOT_TOP_LEVEL]);
    }

    #[test]
    fn flags_require_inside_local_function_as_not_top_level() {
        let (sites, diagnostics) = scan("local function f()\n  require(\"x\")\nend\n");
        assert!(sites.is_empty());
        assert_eq!(codes_of(&diagnostics), vec![codes::REQUIRE_NOT_TOP_LEVEL]);
    }

    // --- require-dynamic: 引数が文字列リテラルでない ---

    #[test]
    fn flags_dynamic_argument_in_binding_form() {
        let (sites, diagnostics) = scan("local key = \"x\"\nlocal a = require(key)\n");
        assert!(sites.is_empty());
        assert_eq!(codes_of(&diagnostics), vec![codes::REQUIRE_DYNAMIC]);
    }

    #[test]
    fn flags_dynamic_argument_in_bare_form() {
        let (sites, diagnostics) = scan("local key = \"x\"\nrequire(key)\n");
        assert!(sites.is_empty());
        assert_eq!(codes_of(&diagnostics), vec![codes::REQUIRE_DYNAMIC]);
    }

    // --- severity ---

    #[test]
    fn require_violations_are_errors() {
        let (_sites, diagnostics) = scan("require(v)\n");
        assert_eq!(diagnostics[0].severity, Severity::Error);
        assert_eq!(diagnostics[0].module.as_deref(), Some("m"));
    }

    // --- 非 require 呼び出しは無視される ---

    #[test]
    fn does_not_flag_unrelated_calls_or_local_require_name() {
        let (sites, diagnostics) = scan("local x = foo(\"a\")\nfoo.require(\"b\")\n");
        assert!(sites.is_empty());
        assert!(diagnostics.is_empty(), "{:?}", codes_of(&diagnostics));
    }
}
