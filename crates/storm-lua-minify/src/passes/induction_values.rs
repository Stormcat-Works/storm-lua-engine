//! 整数 numeric-for 誘導変数の floor 呼び出し省略（TS `passes/induction-values.ts` の移植）。
//!
//! Lua は初期値・上限・刻みが整数の numeric-for 誘導変数を整数として保持する。
//! Stormworks はこの Lua 挙動に従うため、そのループ内では `math.floor(i)` == `i`。
//! binding/effect 解析に依存する（`resolveBuiltinReference` で `math.floor` を同定）。

use crate::pass::PassResult;
use storm_lua_analysis::effects::EffectAnalyzer;
use storm_lua_analysis::resolver::{resolve, Resolution};
use storm_lua_syntax::ast::{Ast, Node, NodeId};
use storm_lua_syntax::numeric::num_val;

/// TS `integerLiteral`: num かつ safe integer。
fn integer_literal(node: &Node) -> bool {
    match node {
        Node::Num(s) => {
            let v = num_val(s);
            v.is_finite() && v.fract() == 0.0 && v.abs() <= 9_007_199_254_740_991.0
        }
        _ => false,
    }
}

/// 誘導変数 bid の収集。整数初期値・上限・刻みの Fornum の node_bid を集める。
fn collect_integer_bids(ast: &Ast, res: &Resolution, root: NodeId) -> Vec<bool> {
    let mut out = vec![false; res.bindings.len()];
    storm_lua_syntax::ast_utils::walk(ast, root, &mut |id| {
        let Node::Fornum(_, a, b, c, body) = ast.node(id) else {
            return;
        };
        if integer_literal(ast.node(*a))
            && integer_literal(ast.node(*b))
            && c.map(|x| integer_literal(ast.node(x))).unwrap_or(true)
        {
            if let Some(bid) = res.node_bid.get(id as usize).copied().flatten() {
                // A body assignment changes subsequent observations of the
                // induction variable, even when the loop header is integral.
                let mut written = false;
                storm_lua_syntax::ast_utils::walk(ast, *body, &mut |child| {
                    if !written
                        && matches!(ast.node(child), Node::Name(_))
                        && res.node_bid.get(child as usize).copied().flatten() == Some(bid)
                        && res.node_write.get(child as usize).copied().unwrap_or(false)
                    {
                        written = true;
                    }
                });
                if !written {
                    out[bid as usize] = true;
                }
            }
        }
    });
    out
}

/// `foldIntegerInductionCalls` の移植（TS cloneNode 方式）。
///
/// 元 AST を不変参照のまま解析し、書き換えた新 AST を返す。
/// 返り値は (new_ast, new_root, folded 数)。
fn fold_integer_induction_calls_impl(
    ast: &Ast,
    res: &Resolution,
    root: NodeId,
) -> (Ast, NodeId, usize) {
    let integer_bids = collect_integer_bids(ast, res, root);
    let analyzer = EffectAnalyzer::new(ast, res, root, true);
    let mut folded = 0usize;

    let mut new_ast = storm_lua_syntax::ast_utils::inherit_ast(ast);
    new_ast.nodes = ast.nodes.clone();
    let new_root = rewrite(
        &mut new_ast,
        ast,
        &analyzer,
        res,
        &integer_bids,
        root,
        &mut folded,
    );
    (new_ast, new_root, folded)
}

/// `foldIntegerInductionCalls` の公開エントリ（PassFn 互換）。
/// resolution を内部で実行し、書き換え結果を `ast` へ反映して PassResult を返す。
pub fn fold_integer_induction_calls(ast: &mut Ast, root: NodeId) -> PassResult {
    let res = resolve(ast, root);
    let (new_ast, new_root, folded) = fold_integer_induction_calls_impl(ast, &res, root);
    ast.nodes = new_ast.nodes;
    PassResult {
        root: new_root,
        saved: Some(folded as u64),
        details: None,
    }
}

/// 再帰的書き換え。`source_ast` は解析用の不変参照、`new_ast` は書き換え先（TS mapNode）。
fn rewrite(
    new_ast: &mut Ast,
    source_ast: &Ast,
    analyzer: &EffectAnalyzer,
    res: &Resolution,
    integer_bids: &[bool],
    id: NodeId,
    folded: &mut usize,
) -> NodeId {
    let node = new_ast.nodes[id as usize].clone();
    match &node {
        Node::Call(fn_, args, method) => {
            let new_fn = rewrite(
                new_ast,
                source_ast,
                analyzer,
                res,
                integer_bids,
                *fn_,
                folded,
            );
            let mut new_args = Vec::with_capacity(args.len());
            for a in args {
                new_args.push(rewrite(
                    new_ast,
                    source_ast,
                    analyzer,
                    res,
                    integer_bids,
                    *a,
                    folded,
                ));
            }
            // 引数 1 つ・メソッドなし・fn が math.floor・引数が誘導変数のとき fold
            if new_args.len() == 1
                && method.is_none()
                && analyzer.resolve_builtin_reference(*fn_) == Some("math.floor".to_string())
            {
                let argument = new_args[0];
                let is_induction = is_induction_name(source_ast, res, argument, integer_bids);
                if is_induction {
                    *folded += 1;
                    return argument;
                }
            }
            let new_node = Node::Call(new_fn, new_args, method.clone());
            new_ast
                .nodes
                .rewrite(id, new_node, "integer-loop-call-folding");
            id
        }
        _ => {
            let (new_node, _) = storm_lua_syntax::ast_utils::map_children(&node, &mut |c| {
                rewrite(new_ast, source_ast, analyzer, res, integer_bids, c, folded)
            });
            new_ast
                .nodes
                .rewrite(id, new_node, "integer-loop-call-folding");
            id
        }
    }
}

/// 引数が誘導変数の name かどうか（node_bid で判定）。
fn is_induction_name(ast: &Ast, res: &Resolution, id: NodeId, integer_bids: &[bool]) -> bool {
    if !matches!(ast.node(id), Node::Name(_)) {
        return false;
    }
    match res.node_bid.get(id as usize).copied().flatten() {
        Some(bid) if (bid as usize) < integer_bids.len() => integer_bids[bid as usize],
        _ => false,
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use storm_lua_syntax::parser::parse_source;
    use storm_lua_syntax::print::Printer;

    /// 整数誘導変数は fold され、非整数初期値はそのまま残る。
    #[test]
    fn handles_more_bindings_than_ast_nodes() {
        let names = (0..80)
            .map(|index| format!("a{index}"))
            .collect::<Vec<_>>()
            .join(",");
        let source = format!("local {names}\nfor i=1,1 do\noutput.setNumber(1,math.floor(i))\nend");
        let (mut ast, root) = parse_source(&source).expect("parse ok");
        let res = fold_integer_induction_calls(&mut ast, root);
        assert_eq!(res.saved, Some(1));
    }

    #[test]
    fn folds_only_integer_induction_variables() {
        let (mut ast, root) = parse_source(
            "for i = 1, 10 do\na = math.floor(i)\nend\nfor i = 1.5, 10 do\nc = math.floor(i)\nend\nfor i = 9007199254740992, 9007199254740992 do\nd = math.floor(i)\nend",
        )
        .expect("parse ok");
        let res = fold_integer_induction_calls(&mut ast, root);
        assert_eq!(res.saved, Some(1), "fold は 1 件");
        let out = Printer::new(&ast, false).output(res.root);
        assert!(out.contains("a=i"), "got: {out}");
        assert_eq!(
            out.matches("math.floor(i)").count(),
            2,
            "fractional and >MAX_SAFE_INTEGER loops must remain: {out}"
        );
    }
}
