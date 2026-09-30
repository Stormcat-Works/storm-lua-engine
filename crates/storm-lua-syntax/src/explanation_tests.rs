#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
use crate::explanation::ReasonOperation;
use crate::provenance::GeneratedOrigins;
use crate::{parse_source_with_origins, Ast, Node, NodeId, Printer};
fn output(ast: &Ast, root: NodeId) -> GeneratedOrigins {
    let printed = Printer::new(ast, false).output_with_positions(root);
    let result = GeneratedOrigins::from_print(ast, &printed).unwrap();
    result.validate_for_code(&printed.code).unwrap();
    result
}
#[test]
fn rejected_block_rewrite_does_not_leak_disposition_history() {
    let (mut ast, root) = parse_source_with_origins("rollback.lua", "first=1 second=2").unwrap();
    let before = output(&ast, root);
    let snapshot = ast.nodes.capture_origin(root);
    let old = ast.node(root).clone();
    let Node::Block(ref statements) = old else {
        panic!()
    };
    ast.nodes.rewrite(
        root,
        Node::Block(vec![statements[1]]),
        "test-discarded-trial",
    );
    assert!(!output(&ast, root).dispositions.is_empty());
    ast.nodes[root as usize] = old;
    ast.nodes.finish_rewrite(root, snapshot, "test-restore");
    assert_eq!(output(&ast, root), before);
}
#[test]
fn failed_candidate_clone_does_not_change_base_reason_or_removal_records() {
    let (base, root) = parse_source_with_origins("candidate.lua", "local v=2*3 return v").unwrap();
    let original = output(&base, root);
    let mut trial = base.clone();
    let bin = trial
        .nodes
        .iter()
        .position(|n| matches!(n, Node::Bin(..)))
        .unwrap() as NodeId;
    trial.nodes.rewrite(bin, Node::Num("6".into()), "test-fold");
    assert!(output(&trial, root)
        .dispositions
        .iter()
        .any(|d| d.reason.operation == ReasonOperation::Rewrite));
    assert_eq!(output(&base, root), original);
}
#[test]
fn copied_contexts_and_dispositions_remap_to_their_original_snapshot() {
    let source = "return wrap(2*3)";
    let (mut from, _) = parse_source_with_origins("same.lua", source).unwrap();
    let bin = from
        .nodes
        .iter()
        .position(|n| matches!(n, Node::Bin(..)))
        .unwrap() as NodeId;
    let call = from
        .nodes
        .iter()
        .position(|n| matches!(n, Node::Call(..)))
        .unwrap() as NodeId;
    from.nodes.add_inline_context(bin, bin, call);
    from.nodes.rewrite(bin, Node::Num("6".into()), "test-fold");
    let (mut to, root) = parse_source_with_origins("same.lua", "return 0").unwrap();
    let target = to
        .nodes
        .iter()
        .position(|n| matches!(n, Node::Num(_)))
        .unwrap() as NodeId;
    to.nodes[target as usize] = Node::Num("6".into());
    to.nodes.derive_from(target, &from.nodes, bin, "test-copy");
    let result = output(&to, root);
    assert_eq!(result.sources.len(), 2);
    let context = &result.contexts[0];
    assert_eq!(context.definition.source, 1);
    assert_eq!(context.call_site.source, 1);
    assert_eq!(
        &result.sources[1].text[context.definition.start..context.definition.end],
        "2*3"
    );
    assert!(result.dispositions.iter().any(|d| d.original.source == 1
        && &result.sources[1].text[d.original.start..d.original.end] == "2*3"));
    let recovered: Ast = serde_json::from_slice(&serde_json::to_vec(&to).unwrap()).unwrap();
    assert_eq!(output(&recovered, root), result);
}
#[test]
fn malformed_serialized_contexts_are_rejected_before_emission() {
    let (mut ast, root) = parse_source_with_origins("bad.lua", "return f(2*3)").unwrap();
    let bin = ast
        .nodes
        .iter()
        .position(|n| matches!(n, Node::Bin(..)))
        .unwrap() as NodeId;
    let call = ast
        .nodes
        .iter()
        .position(|n| matches!(n, Node::Call(..)))
        .unwrap() as NodeId;
    ast.nodes.add_inline_context(bin, bin, call);
    let mut json = serde_json::to_value(&ast).unwrap();
    json["origins"]["slots"][bin as usize]["contexts"][0]["callSite"]["source"] =
        serde_json::json!(999);
    assert!(serde_json::from_value::<Ast>(json).is_err());
    assert!(!output(&ast, root).contexts.is_empty());
}
#[test]
fn exact_copy_does_not_claim_normalized_literals_are_byte_identical() {
    let source = "return 0x10";
    let (ast, root) = parse_source_with_origins("literal.lua", source).unwrap();
    let printed = Printer::new(&ast, false).output_with_positions(root);
    assert_eq!(printed.code, "return 16");
    let result = GeneratedOrigins::from_print(&ast, &printed).unwrap();
    let at = printed.code.find("16").unwrap();
    let m = result
        .mappings
        .iter()
        .find(|m| m.start <= at && at < m.end)
        .unwrap();
    assert!(m.copied.is_none());
    let origin = &result.origins[m.origin.unwrap() as usize];
    assert!(origin
        .reasons
        .iter()
        .any(|r| r.code.as_ref() == "numeric-literal-spelling"));
}

#[test]
fn syntax_keywords_belong_to_their_actual_token_not_another_keyword() {
    use crate::provenance::OriginPrecision;
    for source in [
        "while flag do output.setNumber(1,7)end",
        "for index=1,8 do output.setNumber(index,index)end",
        "for key,value in pairs(values)do output.setNumber(key,value)end",
        "if flag then output.setNumber(1,7)else output.setNumber(2,8)end",
        "repeat output.setNumber(1,7)until flag",
        "while flag do end",
        "if flag then end",
        "for i=1,3 do end",
    ] {
        let (ast, root) = parse_source_with_origins("keywords.lua", source).unwrap();
        let printed = Printer::new(&ast, false).output_with_positions(root);
        let origins = GeneratedOrigins::from_print(&ast, &printed).unwrap();
        origins.validate_for_code(&printed.code).unwrap();
        for mapping in &origins.mappings {
            let Some(origin) = mapping.origin.map(|id| &origins.origins[id as usize]) else {
                continue;
            };
            if origin.precision == OriginPrecision::Token {
                let span = origin.primary.unwrap();
                assert_eq!(
                    &printed.code[mapping.start..mapping.end],
                    &source[span.start..span.end],
                    "{source}"
                );
                assert!(mapping.copied.is_some());
            }
        }
        let body = if source.starts_with("if") {
            "then"
        } else if source.starts_with("repeat") {
            "until"
        } else {
            "do"
        };
        let offset = printed.code.find(body).unwrap();
        let mapping = origins
            .mappings
            .iter()
            .find(|m| m.start <= offset && offset < m.end)
            .unwrap();
        let origin = &origins.origins[mapping.origin.unwrap() as usize];
        assert_eq!(origin.precision, OriginPrecision::Token, "{source}");
        let span = origin.primary.unwrap();
        assert_eq!(&source[span.start..span.end], body);
    }
}

#[test]
fn selected_empty_root_keeps_its_removals_without_emitting_a_fake_position() {
    let source = "local unused=99";
    let (mut ast, root) = parse_source_with_origins("empty.lua", source).unwrap();
    ast.nodes
        .retain_block_statements(|_| false, "test-unused-declaration");
    let printed = Printer::new(&ast, false).output_with_positions(root);
    assert!(printed.code.is_empty());
    assert!(printed.emissions.is_empty());
    let origins = GeneratedOrigins::from_print(&ast, &printed).unwrap();
    origins.validate_for_code("").unwrap();
    assert!(origins.mappings.is_empty());
    assert!(origins.constructs.is_empty());
    assert_eq!(origins.dispositions.len(), 1);
    assert_eq!(
        &source[origins.dispositions[0].original.start..origins.dispositions[0].original.end],
        source
    );
}

#[test]
fn repeated_rule_keeps_different_observed_decisions() {
    use crate::explanation::OptimizationReason;
    let (mut ast, root) = parse_source_with_origins("same-rule.lua", "return 7").unwrap();
    let id = ast
        .nodes
        .iter()
        .position(|n| matches!(n, Node::Num(_)))
        .unwrap() as NodeId;
    for value in ["first", "second"] {
        ast.nodes.explain(
            id,
            OptimizationReason::decision("test-rule", "checked-input", [("value", value.into())]),
        );
    }
    ast.nodes.rewrite(id, Node::Num("8".into()), "test-rule");
    let result = output(&ast, root);
    let origin = result
        .origins
        .iter()
        .find(|o| o.reasons.iter().filter(|r| r.basis.is_some()).count() == 2)
        .unwrap();
    assert_eq!(
        origin
            .reasons
            .iter()
            .filter_map(|r| r.facts.first().map(|f| f.value.as_ref()))
            .collect::<Vec<_>>(),
        ["first", "second"]
    );
}
