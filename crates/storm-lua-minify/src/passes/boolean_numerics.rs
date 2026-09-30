//! numeric-boolean-bit-specialization パス（TS `passes/boolean-numerics.ts` の移植）。
//!
//! `bit = (value & mask) > 0` で定義されたビットテストを、その boolean 値を
//! `0/1` に変換する場所（`bit and yes or no` 形の消費者）がすべての場合に、
//! 定義式を `(value >> shift) & 1` 相当へ置換してサイズを削減する。
//!
//! 意味保存の証明条件:
//! - Lua の比較演算は boolean を返すため、定義だけを数値へ置換してはならない。
//! - mask = 2^shift のとき `(value >> shift) & 1` は対象ビットを 0/1 で返す。
//! - ビット比較がローカル宣言の初期化式であり、そのbindingへの後続writeがないこと。
//!   assign/globalは支配関係・初期値・前フレーム値を証明できないため対象外。
//! - すべての読み出しが「boolean → 0/1」の変換（`(bit and yes or no)` の形で
//!   yes/no が 0/1）に限られる場合のみ、定義と全消費者を同時に数値へ置換してよい。
//! - 多重ラウンド: 1 回の書き換えが次の候補を生むことがあるため、16 ラウンド上限で
//!   収束するまで繰り返す（TS maxRounds = 16）。
//! - 決定性（NFR-2）: definitions の収集順・同一サイズ時の先勝ち（strict <）を
//!   TS と同一にする。

use crate::pass::PassResult;
use storm_lua_analysis::resolver::{resolve, BindingKind, Resolution};
use storm_lua_syntax::ast::{Ast, Node, NodeId};
use storm_lua_syntax::ast_utils::{inherit_ast, map_children};
use storm_lua_syntax::numeric::{normalize_num_literal, num_val};

struct Definition {
    bid: u32,
    expression: NodeId,
}

struct Candidate {
    ast: Ast,
    root: NodeId,
    size: usize,
    consumers: usize,
}

#[derive(Clone, Copy, PartialEq)]
enum Consumer {
    Direct,
    Inverse,
}

/// `numVal` で数値化（TS と同一）。文字列リテラルを f64 へ。
fn num(s: &str) -> f64 {
    num_val(s)
}

/// TS `numericBooleanConsumer` の移植。`node` が
/// `(name(bid) and yes) or no` かつ yes/no が 0/1 のとき
/// Direct/Inverse を返す。それ以外は None。
fn numeric_boolean_consumer(ast: &Ast, res: &Resolution, id: NodeId, bid: u32) -> Option<Consumer> {
    let Node::Bin(op, l, r) = ast.node(id) else {
        return None;
    };
    if op != "or" {
        return None;
    }
    let Node::Num(rv) = ast.node(*r) else {
        return None;
    };
    let Node::Bin(lop, ll, lr) = ast.node(*l) else {
        return None;
    };
    if lop != "and" {
        return None;
    }
    let Node::Name(_) = ast.node(*ll) else {
        return None;
    };
    if res.node_bid.get(*ll as usize).copied().flatten() != Some(bid) {
        return None;
    }
    let Node::Num(lrv) = ast.node(*lr) else {
        return None;
    };
    let yes = num(lrv);
    let no = num(rv);
    if yes == 1.0 && no == 0.0 {
        return Some(Consumer::Direct);
    }
    if yes == 0.0 && no == 1.0 {
        return Some(Consumer::Inverse);
    }
    None
}

/// TS `numericBit` の移植。`id` が `(value & mask) > 0` の形で
/// mask が 2 のべき乗のとき、`(value, shift)` を返す。それ以外は None。
fn numeric_bit(ast: &Ast, id: NodeId) -> Option<(NodeId, u32)> {
    let Node::Bin(op, l, r) = ast.node(id) else {
        return None;
    };
    if op != ">" {
        return None;
    }
    let Node::Num(rv) = ast.node(*r) else {
        return None;
    };
    if num(rv) != 0.0 {
        return None;
    }
    let Node::Bin(lop, ll, lr) = ast.node(*l) else {
        return None;
    };
    if lop != "&" {
        return None;
    }
    let Node::Num(lrv) = ast.node(*lr) else {
        return None;
    };
    let mask = num(lrv);
    let shift = mask.log2();
    if !shift.is_finite() || !(0.0..=52.0).contains(&shift) || shift.fract() != 0.0 {
        return None;
    }
    Some((*ll, shift as u32))
}

/// ラウンド1回分の解析（TS の collect）。write 回数と定義候補を集める。
fn collect(ast: &Ast, res: &Resolution, root: NodeId) -> (Vec<u32>, Vec<Definition>) {
    let mut writes: Vec<u32> = vec![0; res.bindings.len()];
    let mut definitions: Vec<Definition> = Vec::new();
    storm_lua_syntax::ast_utils::walk(ast, root, &mut |id| {
        if res.node_write[id as usize] {
            if let Some(b) = res.node_bid[id as usize] {
                writes[b as usize] += 1;
            }
        }
        match ast.node(id) {
            // A local declaration dominates every read of its binding. Assignments
            // may be conditional, follow an earlier read, or preserve a previous
            // callback value, so they require CFG dominance data we do not have.
            Node::Local(names, es) if names.len() == es.len() => {
                let bids = res.node_bids.get(id as usize).cloned().unwrap_or_default();
                for (index, &e) in es.iter().enumerate() {
                    if let Some(&bid) = bids.get(index) {
                        if numeric_bit(ast, e).is_some() {
                            definitions.push(Definition { bid, expression: e });
                        }
                    }
                }
            }
            _ => {}
        }
    });
    (writes, definitions)
}

/// TS `validate` の移植。定義 bid の読み出しがすべて消費者か検査する。
fn validate(
    ast: &Ast,
    res: &Resolution,
    def_bid: u32,
    id: NodeId,
    consumers: &mut usize,
    invalid: &mut bool,
) {
    if *invalid {
        return;
    }
    if numeric_boolean_consumer(ast, res, id, def_bid).is_some() {
        *consumers += 1;
        return;
    }
    if matches!(ast.node(id), Node::Name(_))
        && res.node_bid[id as usize] == Some(def_bid)
        && !res.node_write[id as usize]
    {
        *invalid = true;
        return;
    }
    storm_lua_analysis::resolver::for_each_child(ast, id, &mut |c| {
        validate(ast, res, def_bid, c, consumers, invalid);
    });
}

/// 候補 AST を構築する（TS `rewrite` の移植）。def.expression を
/// `(value >> shift) & 1` 相当へ置換し、消費者を直接の数値表現へ置換する。
fn build_candidate(
    ast: &Ast,
    root: NodeId,
    res: &Resolution,
    def: &Definition,
    binding_name: &str,
) -> (Ast, NodeId) {
    let mut new_ast = inherit_ast(ast);
    new_ast.nodes = ast.nodes.clone();
    #[expect(
        clippy::expect_used,
        reason = "Definition collection validated this numeric-bit expression and the source arena has not changed"
    )]
    let (value_id, shift) = numeric_bit(ast, def.expression).expect("collect で検証済み");
    let generated = new_ast.nodes.len();
    let one = new_ast.push(Node::Num("1".into()));
    let replacement = if shift == 0 {
        new_ast.bin("&", value_id, one)
    } else {
        let shift_lit = new_ast.push(Node::Num(normalize_num_literal(&shift.to_string()).into()));
        let shifted = new_ast.bin(">>", value_id, shift_lit);
        new_ast.bin("&", shifted, one)
    };
    if new_ast.nodes.tracks_origins() {
        let Node::Bin(_, masked, zero) = ast.node(def.expression) else {
            unreachable!()
        };
        let Node::Bin(_, _, mask) = ast.node(*masked) else {
            unreachable!()
        };
        for n in generated..new_ast.nodes.len() {
            super::origins::derive(
                &mut new_ast,
                n as NodeId,
                ast,
                &[*mask, *zero],
                "numeric-bit-representation",
            );
        }
        new_ast.nodes.derive_from(
            replacement,
            &ast.nodes,
            def.expression,
            "numeric-bit-definition",
        );
    }
    let rewriter = Rewriter {
        ast,
        res,
        def_bid: def.bid,
        def_expr: def.expression,
        replacement,
        binding_name,
    };
    let new_root = rewriter.rewrite(&mut new_ast, root);
    (new_ast, new_root)
}

struct Rewriter<'a> {
    ast: &'a Ast,
    res: &'a Resolution,
    def_bid: u32,
    def_expr: NodeId,
    replacement: NodeId,
    binding_name: &'a str,
}

impl Rewriter<'_> {
    fn rewrite(&self, new_ast: &mut Ast, id: NodeId) -> NodeId {
        if id == self.def_expr {
            return self.replacement;
        }
        let node = self.ast.node(id).clone();
        if let Some(consumer) = numeric_boolean_consumer(self.ast, self.res, id, self.def_bid) {
            let sym = new_ast.strings.intern(self.binding_name);
            let name_id = new_ast.push(Node::Name(sym));
            let Node::Bin(_, left, no) = self.ast.node(id) else {
                unreachable!()
            };
            let Node::Bin(_, name, yes) = self.ast.node(*left) else {
                unreachable!()
            };
            new_ast
                .nodes
                .derive_from(name_id, &self.ast.nodes, *name, "numeric-bit-read");
            new_ast
                .nodes
                .relate_from(name_id, &self.ast.nodes, id, "numeric-bit-consumer");
            return match consumer {
                Consumer::Direct => name_id,
                Consumer::Inverse => {
                    let one = new_ast.push(Node::Num("1".into()));
                    super::origins::derive(
                        new_ast,
                        one,
                        self.ast,
                        &[*yes, *no],
                        "numeric-bit-inversion-unit",
                    );
                    let result = new_ast.bin("-", one, name_id);
                    new_ast.nodes.derive_from(
                        result,
                        &self.ast.nodes,
                        id,
                        "numeric-bit-inverse-consumer",
                    );
                    result
                }
            };
        }
        let (new_node, _) = map_children(&node, &mut |child| self.rewrite(new_ast, child));
        new_ast
            .nodes
            .rewrite(id, new_node, "numeric-bit-specialization");
        id
    }
}

/// scopeRename 後のサイズ（TS `measured`）。
fn measured(ast: &Ast, root: NodeId) -> usize {
    let mut copy = inherit_ast(ast);
    copy.nodes = ast.nodes.clone();
    let r = crate::scope_rename::scope_rename_fast(&copy, root);
    storm_lua_syntax::size::measure_size(&r.ast, r.root)
}

/// 1 ラウンド: 改善が見つかれば (new_ast, new_root, consumers) を返す。
fn specialize_round(ast: &Ast, root: NodeId) -> Option<(Ast, NodeId, usize)> {
    let res = resolve(ast, root);
    let (writes, definitions) = collect(ast, &res, root);
    let baseline = measured(ast, root);
    let mut best: Option<Candidate> = None;

    for def in definitions {
        let binding = res.binding(def.bid);
        if binding.fixed || binding.kind != BindingKind::Local || writes[def.bid as usize] != 0 {
            continue;
        }
        let mut consumers = 0usize;
        let mut invalid = false;
        validate(ast, &res, def.bid, root, &mut consumers, &mut invalid);
        if invalid || consumers == 0 {
            continue;
        }
        let binding_name = ast.strings.get(binding.name);
        let (cand, cand_root) = build_candidate(ast, root, &res, &def, binding_name);
        let size = measured(&cand, cand_root);
        let is_better = match &best {
            Some(current) => size < current.size,
            None => true,
        };
        if size < baseline && is_better {
            best = Some(Candidate {
                ast: cand,
                root: cand_root,
                size,
                consumers,
            });
        }
    }
    best.map(|candidate| (candidate.ast, candidate.root, candidate.consumers))
}

/// numeric-boolean-bit-specialization の公開エントリ（TS specializeNumericBooleans）。
pub fn specialize_numeric_booleans(ast: &mut Ast, root: NodeId) -> PassResult {
    let mut current = inherit_ast(ast);
    current.nodes = ast.nodes.clone();
    let mut current_root = root;
    let mut specialized = 0u64;
    for _ in 0..16 {
        match specialize_round(&current, current_root) {
            Some((na, nr, n)) => {
                current = na;
                current_root = nr;
                specialized += n as u64;
            }
            None => break,
        }
    }
    ast.nodes = current.nodes;
    PassResult {
        root: current_root,
        saved: Some(specialized),
        details: None,
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::provenance_audit_support::parse_source;
    use crate::provenance_audit_support::Printer;

    #[test]
    fn specializes_direct_and_inverse_consumers() {
        let (mut ast, root) = parse_source("function onTick()local bits=input.getNumber(1)local flag=bits&256>0 local inverse=bits&128>0 output.setNumber(1,flag and 1 or 0)output.setNumber(2,inverse and 0 or 1)end").expect("parse ok");
        let res = specialize_numeric_booleans(&mut ast, root);
        assert_eq!(res.saved, Some(2));
        let out = Printer::new(&ast, false).output(res.root);
        assert!(out.contains("bits>>8&1"), "{out}");
        assert!(out.contains("bits>>7&1"), "{out}");
        assert!(out.contains("setNumber(1,flag)"), "{out}");
        assert!(out.contains("setNumber(2,1-inverse)"), "{out}");
    }

    #[test]
    fn skips_when_consumer_uses_other_values() {
        let (mut ast, root) =
            parse_source("local a={}\nlocal up=a.tick&1>0\nlocal s=up and 5 or 0\nlocal x=s+1")
                .expect("parse ok");
        let res = specialize_numeric_booleans(&mut ast, root);
        assert_eq!(res.saved, Some(0));
    }

    #[test]
    fn rejects_reassigned_or_non_dominating_bindings() {
        for source in [
            "local a={} local bit=a.x&8>0 bit=false local out=bit and 1 or 0 return out",
            "local before=global_bit and 1 or 0 global_bit=input.getNumber(1)&256>0 local after=global_bit and 1 or 0 return before+after",
            "if c then global_bit=input.getNumber(1)&256>0 end local out=global_bit and 1 or 0 return out",
        ] {
            let (mut ast, root) = parse_source(source).expect("parse ok");
            let before = Printer::new(&ast, false).output(root);
            let res = specialize_numeric_booleans(&mut ast, root);
            assert_eq!(res.saved, Some(0), "{source}");
            assert_eq!(Printer::new(&ast, false).output(res.root), before, "{source}");
        }
    }

    #[test]
    fn preserves_parenthesized_bit_expression() {
        let (mut ast, root) =
            parse_source("local a={} local up=(a.tick&1)>0 local s=up and 1 or 0 return s")
                .expect("parse ok");
        let before = Printer::new(&ast, false).output(root);
        let res = specialize_numeric_booleans(&mut ast, root);
        assert_eq!(res.saved, Some(0));
        assert_eq!(Printer::new(&ast, false).output(res.root), before);
    }

    #[test]
    fn round_selects_smallest_candidate_by_size() {
        let (ast, root) = parse_source("local a={} local first=a.x&2>0 local f=first and 1 or 0 local second=a.y&8>0 local x=second and 1 or 0 local y=second and 1 or 0 return f+x+y").expect("parse ok");
        let (candidate, candidate_root, consumers) =
            specialize_round(&ast, root).expect("candidate");
        let out = Printer::new(&candidate, false).output(candidate_root);
        assert_eq!(consumers, 2, "{out}");
        assert!(out.contains("first=a.x&2>0"), "{out}");
        assert!(out.contains("second=a.y>>3&1"), "{out}");
    }
}
