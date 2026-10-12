//! Web UI 用の軽量 property 検出。
//!
//! 正規表現による `property.getNumber("key")` / `property.getBool("key")`
//! / `property.getText("key")` 走査はコメントや文字列リテラル内でも誤検出する。
//! ここでは実際に parse した AST を `EffectAnalyzer::resolve_builtin_reference`
//! で解決し、グローバルな `property` オブジェクトへの呼び出しのみを対象にする。
//! key がグローバル `property` テーブルの呼び出しでない（例: shadow された
//! ローカル変数）場合は検出しない。
//!
//! 直接呼び出しの key 文字列リテラルに加えて、以下の間接パターンも検出する
//! （ただし `passes::property_reads` のハードコード対象は直接呼び出しのみで、
//! この非対称性は Web UI の警告文で案内する）:
//!
//! - key の定数伝播: `local KEY = "Foo"` の後の `property.getNumber(KEY)`。
//!   `KEY` が宣言以降一度も再代入されていない場合のみ解決する（transitive）。
//! - 1段の関数引数passthrough: `f("Lit", ...)` で `f` が再代入されない
//!   ローカル関数を指し、その本体が `property.getX(p)`（`p` は書き換えられない
//!   仮引数）を含む場合、呼び出し側の文字列リテラル引数を検出する。

use serde::Serialize;

use crate::effects::EffectAnalyzer;
use crate::resolver::{resolve, BindingId, BindingKind, Resolution};
use storm_lua_syntax::ast::{Ast, Node, NodeId};
use storm_lua_syntax::ast_utils::walk;
use storm_lua_syntax::numeric::decode_lua_string;
use storm_lua_syntax::parser::parse_source;

/// `scan_properties` の結果。
///
/// `ok == false` の場合は parse に失敗しており、`numbers` / `bools` / `texts` /
/// `dynamic_count` は常に空・0。呼び出し側（Web UI）は正規表現ベースの
/// fallback へ切り替える。
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PropertyScanResult {
    /// Whether the source was parsed successfully for property scanning.
    pub ok: bool,
    /// `property.getNumber("key")` で検出された key。初出順・重複なし。
    pub numbers: Vec<String>,
    /// `property.getBool("key")` で検出された key。初出順・重複なし。
    pub bools: Vec<String>,
    /// `property.getText("key")` で検出された key。初出順・重複なし。
    pub texts: Vec<String>,
    /// key 引数が文字列リテラルでも定数伝播/passthroughでも解決できない
    /// （動的 key）呼び出しの件数。
    pub dynamic_count: usize,
}

enum PropertyKind {
    Number,
    Bool,
    Text,
}

fn builtin_kind(name: &str) -> Option<PropertyKind> {
    match name {
        "property.getNumber" => Some(PropertyKind::Number),
        "property.getBool" => Some(PropertyKind::Bool),
        "property.getText" => Some(PropertyKind::Text),
        _ => None,
    }
}

/// key 引数ノードを文字列リテラルへ解決する。直接リテラル、または
/// 「宣言以降再代入されていないローカル変数の初期化子」を辿る（transitive）。
fn resolve_key_literal(ast: &Ast, resolution: &Resolution, node: NodeId) -> Option<String> {
    resolve_key_literal_depth(ast, resolution, node, 8)
}

fn resolve_key_literal_depth(
    ast: &Ast,
    resolution: &Resolution,
    node: NodeId,
    depth: u32,
) -> Option<String> {
    if depth == 0 {
        return None;
    }
    match ast.node(node) {
        Node::Str(raw) => decode_lua_string(raw),
        Node::Name(_) => {
            let bid = resolution.node_bid[node as usize]?;
            let binding = resolution.binding(bid);
            if binding.kind != BindingKind::Local {
                return None;
            }
            if resolution.binding_write_counts[bid as usize] != 0 {
                return None;
            }
            let decl = binding.decl_node?;
            let Node::Local(_, es) = ast.node(decl) else {
                return None;
            };
            let bids = &resolution.node_bids[decl as usize];
            let idx = bids.iter().position(|&b| b == bid)?;
            let init = *es.get(idx)?;
            resolve_key_literal_depth(ast, resolution, init, depth - 1)
        }
        _ => None,
    }
}

/// `bid` が「再代入されないローカル関数（`local function` または
/// `local f = function ... end`）」を指す場合、その関数ノードを返す。
fn resolve_local_function(ast: &Ast, resolution: &Resolution, bid: BindingId) -> Option<NodeId> {
    let binding = resolution.binding(bid);
    if resolution.binding_write_counts[bid as usize] != 0 {
        return None;
    }
    match binding.kind {
        BindingKind::Localfunc => binding.function_node,
        BindingKind::Local => {
            let decl = binding.decl_node?;
            let Node::Local(_, es) = ast.node(decl) else {
                return None;
            };
            let bids = &resolution.node_bids[decl as usize];
            let idx = bids.iter().position(|&b| b == bid)?;
            let init = *es.get(idx)?;
            match ast.node(init) {
                Node::Function(..) => Some(init),
                _ => None,
            }
        }
        _ => None,
    }
}

/// `fn_node`（`Node::Function`）の本体を走査し、各仮引数（書き換えられない
/// もののみ）位置ごとに検出される property 種別を集める。仮引数の位置は
/// 呼び出し側の引数位置と対応する。
fn param_property_kinds(
    ast: &Ast,
    resolution: &Resolution,
    analyzer: &EffectAnalyzer<'_>,
    fn_node: NodeId,
) -> Vec<(usize, PropertyKind)> {
    let Node::Function(_, _, body) = ast.node(fn_node) else {
        return Vec::new();
    };
    let param_bids = resolution.node_bids[fn_node as usize].clone();
    let mut out: Vec<(usize, PropertyKind)> = Vec::new();
    walk(ast, *body, &mut |id| {
        let Node::Call(function, args, _) = ast.node(id) else {
            return;
        };
        let Some(builtin) = analyzer.resolve_builtin_reference(*function) else {
            return;
        };
        let Some(kind) = builtin_kind(&builtin) else {
            return;
        };
        let Some(&arg) = args.first() else {
            return;
        };
        let Node::Name(_) = ast.node(arg) else {
            return;
        };
        let Some(arg_bid) = resolution.node_bid[arg as usize] else {
            return;
        };
        if resolution.binding_write_counts[arg_bid as usize] != 0 {
            return;
        }
        let Some(param_idx) = param_bids.iter().position(|&b| b == arg_bid) else {
            return;
        };
        out.push((param_idx, kind));
    });
    out
}

/// ソースを parse し、グローバル `property` オブジェクトへの
/// `getNumber` / `getBool` / `getText` 呼び出しを AST 上で収集する。
///
/// - コメント・文字列リテラル内の記述は対象外（parse 後の AST しか見ない）
/// - key がローカル変数で shadow された `property` は対象外
/// - key 引数が文字列リテラルでない呼び出しは、定数伝播 / 1段の関数引数
///   passthrough で解決できない限り `dynamic_count` に計上する
pub fn scan_properties(source: &str) -> PropertyScanResult {
    let Ok((ast, root)) = parse_source(source) else {
        return PropertyScanResult::default();
    };
    let resolution = resolve(&ast, root);
    let analyzer = EffectAnalyzer::new(&ast, &resolution, root, true);

    let mut numbers: Vec<String> = Vec::new();
    let mut bools: Vec<String> = Vec::new();
    let mut texts: Vec<String> = Vec::new();
    let mut dynamic_count = 0usize;

    let mut push = |kind: &PropertyKind, key: String| {
        let bucket = match kind {
            PropertyKind::Number => &mut numbers,
            PropertyKind::Bool => &mut bools,
            PropertyKind::Text => &mut texts,
        };
        if !bucket.contains(&key) {
            bucket.push(key);
        }
    };

    walk(&ast, root, &mut |id| {
        let Node::Call(function, args, _) = ast.node(id) else {
            return;
        };

        // 直接 `property.getX(...)` 呼び出し（定数伝播も含む）。
        if let Some(builtin) = analyzer.resolve_builtin_reference(*function) {
            if let Some(kind) = builtin_kind(&builtin) {
                let Some(&arg) = args.first() else {
                    dynamic_count += 1;
                    return;
                };
                if let Some(key) = resolve_key_literal(&ast, &resolution, arg) {
                    push(&kind, key);
                    return;
                }
                // 未書き換えの仮引数は 1段 passthrough 解析（呼び出し側）に
                // 譲る。ここで dynamic 計上すると呼び出し側の集計と二重に
                // なる。書き換えられた仮引数はここで dynamic 計上する。
                let is_unwritten_param = matches!(ast.node(arg), Node::Name(_))
                    && resolution.node_bid[arg as usize].is_some_and(|bid| {
                        resolution.binding(bid).kind == BindingKind::Param
                            && resolution.binding_write_counts[bid as usize] == 0
                    });
                if !is_unwritten_param {
                    dynamic_count += 1;
                }
                return;
            }
        }

        // 1段の関数引数passthrough: `f("Lit", ...)` で f がローカル関数。
        let Node::Name(_) = ast.node(*function) else {
            return;
        };
        let Some(fn_bid) = resolution.node_bid[*function as usize] else {
            return;
        };
        let Some(fn_node) = resolve_local_function(&ast, &resolution, fn_bid) else {
            return;
        };
        let params = param_property_kinds(&ast, &resolution, &analyzer, fn_node);
        if params.is_empty() {
            return;
        }
        for (idx, kind) in &params {
            let Some(&call_arg) = args.get(*idx) else {
                dynamic_count += 1;
                continue;
            };
            match ast.node(call_arg) {
                Node::Str(raw) => match decode_lua_string(raw) {
                    Some(key) => push(kind, key),
                    None => dynamic_count += 1,
                },
                _ => dynamic_count += 1,
            }
        }
    });

    PropertyScanResult {
        ok: true,
        numbers,
        bools,
        texts,
        dynamic_count,
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn detects_number_and_bool_keys_in_order() {
        let result = scan_properties(
            "x=property.getNumber(\"X\") y=property.getBool(\"Enabled\") z=property.getNumber(\"Y\")",
        );
        assert!(result.ok);
        assert_eq!(result.numbers, vec!["X".to_string(), "Y".to_string()]);
        assert_eq!(result.bools, vec!["Enabled".to_string()]);
        assert_eq!(result.dynamic_count, 0);
    }

    #[test]
    fn dedupes_repeated_keys() {
        let result = scan_properties(
            "a=property.getNumber(\"X\") b=property.getNumber(\"X\") c=property.getNumber(\"X\")",
        );
        assert_eq!(result.numbers, vec!["X".to_string()]);
    }

    #[test]
    fn ignores_keys_in_comments() {
        let result =
            scan_properties("-- property.getNumber(\"Fake\")\nx=property.getNumber(\"Real\")");
        assert_eq!(result.numbers, vec!["Real".to_string()]);
    }

    #[test]
    fn ignores_keys_in_string_literals() {
        let result = scan_properties(
            "local s=\"property.getNumber(\\\"Fake\\\")\" x=property.getNumber(\"Real\")",
        );
        assert_eq!(result.numbers, vec!["Real".to_string()]);
    }

    #[test]
    fn ignores_shadowed_local_property() {
        let result = scan_properties(
            "local property={getNumber=function()return 9 end} x=property.getNumber(\"X\")",
        );
        assert!(result.numbers.is_empty());
        assert!(result.bools.is_empty());
    }

    #[test]
    fn counts_dynamic_keys() {
        let result = scan_properties("k=\"X\" x=property.getNumber(k) y=property.getBool(\"Ok\")");
        assert_eq!(result.dynamic_count, 1);
        assert_eq!(result.bools, vec!["Ok".to_string()]);
        assert!(result.numbers.is_empty());
    }

    #[test]
    fn parse_error_returns_not_ok() {
        let result = scan_properties("x = = =");
        assert!(!result.ok);
        assert!(result.numbers.is_empty());
        assert!(result.bools.is_empty());
        assert_eq!(result.dynamic_count, 0);
    }

    #[test]
    fn detects_get_text_keys() {
        let result = scan_properties("x=property.getText(\"Name\") y=property.getNumber(\"X\")");
        assert_eq!(result.texts, vec!["Name".to_string()]);
        assert_eq!(result.numbers, vec!["X".to_string()]);
    }

    #[test]
    fn ignores_get_text_keys_in_comments_and_strings() {
        let result = scan_properties(
            "-- property.getText(\"Fake\")\nlocal s=\"property.getText(\\\"Fake2\\\")\" x=property.getText(\"Real\")",
        );
        assert_eq!(result.texts, vec!["Real".to_string()]);
    }

    #[test]
    fn resolves_constant_propagated_key() {
        let result = scan_properties("local KEY=\"Foo\" x=property.getNumber(KEY)");
        assert_eq!(result.numbers, vec!["Foo".to_string()]);
        assert_eq!(result.dynamic_count, 0);
    }

    #[test]
    fn resolves_transitively_propagated_key() {
        let result = scan_properties("local KEY=\"Foo\" local KEY2=KEY x=property.getNumber(KEY2)");
        assert_eq!(result.numbers, vec!["Foo".to_string()]);
        assert_eq!(result.dynamic_count, 0);
    }

    #[test]
    fn reassigned_local_key_stays_dynamic() {
        let result = scan_properties("local KEY=\"Foo\" KEY=\"Bar\" x=property.getNumber(KEY)");
        assert!(result.numbers.is_empty());
        assert_eq!(result.dynamic_count, 1);
    }

    #[test]
    fn resolves_two_param_function_passthrough() {
        let result = scan_properties(
            "local function createFallbackGetter(primaryKey, secondaryKey)\n\
             local a = property.getNumber(primaryKey)\n\
             local b = property.getBool(secondaryKey)\n\
             return a, b\n\
             end\n\
             createFallbackGetter(\"Primary\", \"Secondary\")",
        );
        assert_eq!(result.numbers, vec!["Primary".to_string()]);
        assert_eq!(result.bools, vec!["Secondary".to_string()]);
        assert_eq!(result.dynamic_count, 0);
    }

    #[test]
    fn passthrough_param_written_inside_fn_stays_dynamic() {
        let result = scan_properties(
            "local function getIt(key)\n\
             key = key\n\
             return property.getNumber(key)\n\
             end\n\
             getIt(\"Lit\")",
        );
        assert!(result.numbers.is_empty());
        assert_eq!(result.dynamic_count, 1);
    }

    #[test]
    fn passthrough_non_literal_call_site_arg_stays_dynamic() {
        let result = scan_properties(
            "local function getIt(key)\n\
             return property.getNumber(key)\n\
             end\n\
             local k = \"Dyn\"\n\
             getIt(k)",
        );
        assert!(result.numbers.is_empty());
        assert_eq!(result.dynamic_count, 1);
    }

    #[test]
    fn nested_call_depth_two_stays_dynamic() {
        let result = scan_properties(
            "local function inner(key)\n\
             return property.getNumber(key)\n\
             end\n\
             local function outer(key)\n\
             return inner(key)\n\
             end\n\
             outer(\"Lit\")",
        );
        assert!(result.numbers.is_empty());
        assert!(result.dynamic_count >= 1);
    }
}
