//! Conditional call/assignment/return lowering (`passes/conditionals.ts`).

use crate::pass::PassResult;
use storm_lua_analysis::effects::{ast_same, EffectAnalyzer};
use storm_lua_analysis::resolver::{resolve, Resolution};
use storm_lua_syntax::ast::{Ast, Node, NodeId};
use storm_lua_syntax::size::{measure_size, measure_stmt};

fn definitely_truthy(ast: &Ast, expression: NodeId, analyzer: &EffectAnalyzer<'_>) -> bool {
    let value = match ast.node(expression) {
        Node::Paren(inner) => *inner,
        _ => expression,
    };
    match ast.node(value) {
        Node::Num(_) | Node::Str(_) | Node::Table(_) | Node::Function(..) => true,
        Node::Bool(value) => *value,
        Node::Un(op, _) => matches!(op.as_str(), "-" | "#" | "~"),
        Node::Bin(op, _, _) => matches!(
            op.as_str(),
            "+" | "-" | "*" | "/" | "//" | "%" | "^" | "&" | "|" | "~" | "<<" | ">>" | ".."
        ),
        Node::Call(function, _, _) => matches!(
            analyzer.resolve_builtin_reference(*function).as_deref(),
            Some(
                "input.getNumber"
                    | "property.getNumber"
                    | "math.abs"
                    | "math.max"
                    | "math.min"
                    | "math.floor"
                    | "math.sin"
                    | "math.cos"
                    | "math.tan"
                    | "math.atan"
                    | "math.sqrt"
                    | "string.format"
            )
        ),
        _ => false,
    }
}

fn logical_ternary(ast: &mut Ast, condition: NodeId, yes: NodeId, no: NodeId) -> NodeId {
    let conjunction = ast.bin("and", condition, yes);
    ast.bin("or", conjunction, no)
}

struct ConditionalBranches {
    condition: NodeId,
    yes: Option<NodeId>,
    no: Option<NodeId>,
    has_else: bool,
}

fn single_statement(ast: &Ast, block: Option<NodeId>) -> Option<NodeId> {
    let block = block?;
    let Node::Block(statements) = ast.node(block) else {
        return None;
    };
    if statements.len() == 1 {
        Some(statements[0])
    } else {
        None
    }
}

fn extract_branches(ast: &Ast, statement: NodeId) -> Option<ConditionalBranches> {
    let Node::If(arms, else_block) = ast.node(statement) else {
        return None;
    };
    if arms.len() != 1 {
        return None;
    }
    Some(ConditionalBranches {
        condition: arms[0].cond,
        yes: single_statement(ast, Some(arms[0].body)),
        no: single_statement(ast, *else_block),
        has_else: else_block.is_some(),
    })
}

fn known_callable(
    ast: &Ast,
    res: &Resolution,
    analyzer: &EffectAnalyzer<'_>,
    function: NodeId,
) -> bool {
    if matches!(ast.node(function), Node::Name(_)) {
        if let Some(bid) = res.node_bid.get(function as usize).copied().flatten() {
            if res.binding(bid).function_node.is_some() {
                return true;
            }
        }
    }
    analyzer.resolve_builtin_reference(function).is_some()
}

fn lower_different_functions_same_args(
    ast: &mut Ast,
    condition: NodeId,
    yes_function: NodeId,
    yes_args: &[NodeId],
    no_function: NodeId,
    no_args: &[NodeId],
) -> Option<NodeId> {
    if !yes_args
        .iter()
        .zip(no_args)
        .all(|(yes, no)| ast_same(ast, *yes, *no))
    {
        return None;
    }
    let selected = logical_ternary(ast, condition, yes_function, no_function);
    let parenthesized = ast.paren(selected);
    let call = ast.call(parenthesized, yes_args.to_vec(), None);
    Some(ast.callstat(call))
}

fn lower_same_function_different_args(
    ast: &mut Ast,
    condition: NodeId,
    function: NodeId,
    yes_args: &[NodeId],
    no_args: &[NodeId],
    analyzer: &EffectAnalyzer<'_>,
) -> Option<NodeId> {
    let condition_reads = analyzer.effects_for_expr(condition).reads;
    let differing = yes_args
        .iter()
        .zip(no_args)
        .enumerate()
        .filter_map(|(index, (yes, no))| (!ast_same(ast, *yes, *no)).then_some(index))
        .collect::<Vec<_>>();
    if differing.len() != 1 {
        return None;
    }
    let index = differing[0];
    // In the original form the condition is evaluated before *any* call
    // argument.  In the lowered form it is embedded in an argument, so an
    // earlier argument must not be able to invalidate a value read by the
    // condition (and an unknown call is conservatively considered capable of
    // doing so).
    if yes_args.iter().any(|argument| {
        let effect = analyzer.effects_for_expr(*argument);
        effect.calls
            || effect.ordered
            || effect
                .writes
                .iter()
                .any(|written| condition_reads.contains(written))
    }) {
        return None;
    }
    if !definitely_truthy(ast, yes_args[index], analyzer) {
        return None;
    }
    let mut args = yes_args.to_vec();
    args[index] = logical_ternary(ast, condition, yes_args[index], no_args[index]);
    let call = ast.call(function, args, None);
    Some(ast.callstat(call))
}

fn call_candidate(
    ast: &mut Ast,
    res: &Resolution,
    analyzer: &EffectAnalyzer<'_>,
    statement: NodeId,
) -> Option<NodeId> {
    let branches = extract_branches(ast, statement)?;
    if !branches.has_else || !analyzer.is_movable(branches.condition) {
        return None;
    }
    let yes = branches.yes?;
    let no = branches.no?;
    let Node::Callstat(yes_expr) = ast.node(yes) else {
        return None;
    };
    let Node::Callstat(no_expr) = ast.node(no) else {
        return None;
    };
    let Node::Call(yes_function, yes_args, yes_method) = ast.node(*yes_expr).clone() else {
        return None;
    };
    let Node::Call(no_function, no_args, no_method) = ast.node(*no_expr).clone() else {
        return None;
    };
    if yes_method.is_some()
        || no_method.is_some()
        || yes_args.len() != no_args.len()
        || !known_callable(ast, res, analyzer, yes_function)
        || !known_callable(ast, res, analyzer, no_function)
    {
        return None;
    }
    if let Some(candidate) = lower_different_functions_same_args(
        ast,
        branches.condition,
        yes_function,
        &yes_args,
        no_function,
        &no_args,
    ) {
        return Some(candidate);
    }
    if ast_same(ast, yes_function, no_function) {
        return lower_same_function_different_args(
            ast,
            branches.condition,
            yes_function,
            &yes_args,
            &no_args,
            analyzer,
        );
    }
    None
}

fn single_assignment(ast: &Ast, statement: Option<NodeId>) -> Option<(NodeId, NodeId)> {
    let statement = statement?;
    let Node::Assign(targets, expressions) = ast.node(statement) else {
        return None;
    };
    if targets.len() != 1
        || expressions.len() != 1
        || !matches!(ast.node(targets[0]), Node::Name(_))
    {
        return None;
    }
    Some((targets[0], expressions[0]))
}

fn prepend_or(ast: &mut Ast, prefix: NodeId, expression: NodeId) -> NodeId {
    if let Node::Bin(op, left, right) = ast.node(expression).clone() {
        if op == "or" {
            let prefix = prepend_or(ast, prefix, left);
            return ast.bin("or", prefix, right);
        }
    }
    ast.bin("or", prefix, expression)
}

fn flat_logical_ternary(ast: &mut Ast, condition: NodeId, yes: NodeId, no: NodeId) -> NodeId {
    let conjunction = ast.bin("and", condition, yes);
    // `or` is associative while preserving left-to-right short-circuit
    // evaluation. Recursively prepend to an already-lowered fallback so long
    // elseif chains do not pay for right-nested parentheses.
    prepend_or(ast, conjunction, no)
}

fn assignment_candidate(
    ast: &mut Ast,
    analyzer: &EffectAnalyzer<'_>,
    statement: NodeId,
) -> Option<NodeId> {
    let Node::If(arms, else_block) = ast.node(statement).clone() else {
        return None;
    };

    // Keep the historical one-arm path unchanged, including its conservative
    // movable-condition proof and no-else self fallback.
    if arms.len() == 1 {
        let branches = extract_branches(ast, statement)?;
        if !analyzer.is_movable(branches.condition) {
            return None;
        }
        let (yes_target, yes_value) = single_assignment(ast, branches.yes)?;
        if !definitely_truthy(ast, yes_value, analyzer) {
            return None;
        }
        let no = if branches.has_else {
            let (target, value) = single_assignment(ast, branches.no)?;
            if !ast_same(ast, yes_target, target) {
                return None;
            }
            Some(value)
        } else {
            None
        };
        let fallback = no.unwrap_or(yes_target);
        let value = logical_ternary(ast, branches.condition, yes_value, fallback);
        return Some(ast.assign(vec![yes_target], vec![value]));
    }

    // An if/elseif/.../else chain that assigns one plain name in every branch
    // can be represented by one short-circuit expression. Unlike call lowering,
    // the conditions do not need to be movable: the LHS is a plain name (so it
    // has no evaluation effects), and the nested and/or expression evaluates
    // each reached condition/value in exactly the original order. Requiring a
    // final else also avoids introducing a write on the original fall-through
    // path.
    let else_statement = single_statement(ast, else_block)?;
    let (target, else_value) = single_assignment(ast, Some(else_statement))?;
    let mut branches = Vec::with_capacity(arms.len());
    for arm in arms {
        let statement = single_statement(ast, Some(arm.body))?;
        let (arm_target, value) = single_assignment(ast, Some(statement))?;
        if !ast_same(ast, target, arm_target) || !definitely_truthy(ast, value, analyzer) {
            return None;
        }
        branches.push((arm.cond, value));
    }

    let mut value = else_value;
    for (condition, yes) in branches.into_iter().rev() {
        value = flat_logical_ternary(ast, condition, yes, value);
    }
    Some(ast.assign(vec![target], vec![value]))
}

fn single_return_value(ast: &Ast, statement: Option<NodeId>) -> Option<NodeId> {
    let statement = statement?;
    let Node::Return(expressions) = ast.node(statement) else {
        return None;
    };
    if expressions.len() != 1 {
        return None;
    }
    let value = expressions[0];
    // A direct call/vararg in the final return slot can produce multiple values.
    // Logical operators always collapse their operands to one value, so lowering
    // those forms would silently change the return arity. Parenthesized calls
    // are already single-valued in Lua and therefore remain eligible.
    if matches!(ast.node(value), Node::Call(..) | Node::Vararg) {
        return None;
    }
    Some(value)
}

fn definitely_truthy_return(ast: &Ast, expression: NodeId) -> bool {
    let value = match ast.node(expression) {
        Node::Paren(inner) => *inner,
        _ => expression,
    };
    match ast.node(value) {
        Node::Num(_) | Node::Str(_) | Node::Table(_) | Node::Function(..) => true,
        Node::Bool(value) => *value,
        Node::Un(op, _) => matches!(op.as_str(), "-" | "#" | "~"),
        Node::Bin(op, _, _) => matches!(
            op.as_str(),
            "+" | "-" | "*" | "/" | "//" | "%" | "^" | "&" | "|" | "~" | "<<" | ">>" | ".."
        ),
        // Calls are intentionally not inferred here. Besides avoiding a fresh
        // resolver/effect-analysis traversal for this size pass, direct calls
        // in return position can carry multiple values and are rejected by
        // `single_return_value` anyway.
        _ => false,
    }
}

fn logical_return_ternary(ast: &mut Ast, condition: NodeId, yes: NodeId, no: NodeId) -> NodeId {
    let conjunction = ast.bin("and", condition, yes);
    // `or` is associative while preserving left-to-right short-circuit
    // evaluation. Flatten a previously lowered fallback so guard-return chains
    // print without a parenthesized right-hand `or` expression.
    if let Node::Bin(op, left, right) = ast.node(no).clone() {
        if op == "or" {
            let prefix = ast.bin("or", conjunction, left);
            return ast.bin("or", prefix, right);
        }
    }
    ast.bin("or", conjunction, no)
}

fn conditional_return_value(
    ast: &mut Ast,
    condition: NodeId,
    yes: NodeId,
    no: NodeId,
) -> Option<NodeId> {
    let mut best: Option<(NodeId, usize)> = None;

    if definitely_truthy_return(ast, yes) {
        let value = logical_return_ternary(ast, condition, yes, no);
        let statement = ast.ret(vec![value]);
        best = Some((statement, measure_stmt(ast, statement)));
    }

    // `not c and no or yes` is the symmetric form. It is useful when the
    // original true-branch may be false/nil but the false-branch is known
    // truthy. Pick the shorter of the two forms when both are valid.
    if definitely_truthy_return(ast, no) {
        let negated = ast.un("not", condition);
        let value = logical_return_ternary(ast, negated, no, yes);
        let statement = ast.ret(vec![value]);
        let length = measure_stmt(ast, statement);
        if best.as_ref().is_none_or(|(_, current)| length < *current) {
            best = Some((statement, length));
        }
    }

    best.map(|(statement, _)| statement)
}

fn full_return_candidate(ast: &mut Ast, statement: NodeId) -> Option<NodeId> {
    let branches = extract_branches(ast, statement)?;
    if !branches.has_else {
        return None;
    }
    let yes = single_return_value(ast, branches.yes)?;
    let no = single_return_value(ast, branches.no)?;
    conditional_return_value(ast, branches.condition, yes, no)
}

fn guard_return_candidate(ast: &mut Ast, statement: NodeId, fallback: NodeId) -> Option<NodeId> {
    let branches = extract_branches(ast, statement)?;
    if branches.has_else {
        return None;
    }
    let yes = single_return_value(ast, branches.yes)?;
    let no = single_return_value(ast, Some(fallback))?;
    conditional_return_value(ast, branches.condition, yes, no)
}

fn attribute_conditional_nodes(
    ast: &mut Ast,
    start: usize,
    statement: NodeId,
    fallback: Option<NodeId>,
    transformation: &str,
) {
    if !ast.nodes.tracks_origins() {
        return;
    }
    let origin = ast.nodes.capture_origin(statement);
    for node in start..ast.nodes.len() {
        ast.nodes
            .finish_rewrite(node as NodeId, origin.clone(), transformation);
        if let Some(fallback) = fallback {
            ast.nodes
                .relate_within(node as NodeId, fallback, transformation);
        }
    }
}

fn transform_return_node(ast: &mut Ast, id: NodeId, lowered: &mut usize) -> NodeId {
    if matches!(ast.node(id), Node::Block(_)) {
        return transform_return_block(ast, id, lowered);
    }
    let node = ast.node(id).clone();
    let (mapped, changed) = storm_lua_syntax::ast_utils::map_children(&node, &mut |child| {
        transform_return_node(ast, child, lowered)
    });
    if changed {
        ast.nodes.rewrite(id, mapped, "conditional-lowering");
    }
    id
}

fn transform_return_block(ast: &mut Ast, block: NodeId, lowered: &mut usize) -> NodeId {
    let Node::Block(statements) = ast.node(block).clone() else {
        unreachable!()
    };

    let statement_count = statements.len();
    let mut output = Vec::with_capacity(statement_count);
    for (index, statement) in statements.into_iter().enumerate() {
        let statement = transform_return_node(ast, statement, lowered);
        // Lua requires a return to be the final statement in its block. An
        // if/else whose arms return may still precede statements or a label
        // reachable by goto; preserve that wrapper rather than dropping the tail.
        let candidate = if index + 1 == statement_count {
            let start = ast.nodes.len();
            let candidate = full_return_candidate(ast, statement);
            attribute_conditional_nodes(ast, start, statement, None, "conditional-return-lowering");
            candidate
        } else {
            None
        };
        let selected = if let Some(candidate) = candidate {
            if measure_stmt(ast, candidate) < measure_stmt(ast, statement) {
                *lowered += 1;
                candidate
            } else {
                statement
            }
        } else {
            statement
        };
        output.push(selected);
    }

    // A tail such as `if c then return 0 end;return 1` can be shortened to a
    // single return. Walk backwards so repeated guard-return chains collapse in
    // the same pass. Compare synthetic blocks to account for statement
    // separators exactly, rather than assuming a fixed delimiter cost.
    if output.len() >= 2 {
        let mut index = output.len() - 1;
        while index > 0 {
            let first = output[index - 1];
            let second = output[index];
            let start = ast.nodes.len();
            let candidate = guard_return_candidate(ast, first, second);
            attribute_conditional_nodes(ast, start, first, Some(second), "guard-return-lowering");
            if let Some(candidate) = candidate {
                let old_block = ast.block(vec![first, second]);
                let new_block = ast.block(vec![candidate]);
                if measure_size(ast, new_block) < measure_size(ast, old_block) {
                    output[index - 1] = candidate;
                    output.remove(index);
                    *lowered += 1;
                }
            }
            index -= 1;
        }
    }

    ast.nodes
        .rewrite(block, Node::Block(output), "conditional-return-lowering");
    block
}

fn process_returns(ast: &mut Ast, root: NodeId) -> PassResult {
    let original = measure_size(ast, root);
    let mut lowered = 0usize;
    let out = transform_return_node(ast, root, &mut lowered);
    let after = measure_size(ast, out);
    PassResult {
        root: out,
        saved: Some(original.saturating_sub(after) as u64),
        details: Some(if lowered == 0 {
            Vec::new()
        } else {
            vec![format!("lowered={lowered}")]
        }),
    }
}

enum ConditionalKind {
    Calls,
    Assignments,
}

fn transform_node(
    ast: &mut Ast,
    res: &Resolution,
    analyzer: &EffectAnalyzer<'_>,
    id: NodeId,
    kind: &ConditionalKind,
    lowered: &mut usize,
) -> NodeId {
    if matches!(ast.node(id), Node::Block(_)) {
        return transform_block(ast, res, analyzer, id, kind, lowered);
    }
    let node = ast.node(id).clone();
    let (mapped, changed) = storm_lua_syntax::ast_utils::map_children(&node, &mut |child| {
        transform_node(ast, res, analyzer, child, kind, lowered)
    });
    if changed {
        ast.nodes.rewrite(id, mapped, "conditional-lowering");
    }
    id
}

fn transform_block(
    ast: &mut Ast,
    res: &Resolution,
    analyzer: &EffectAnalyzer<'_>,
    block: NodeId,
    kind: &ConditionalKind,
    lowered: &mut usize,
) -> NodeId {
    let Node::Block(statements) = ast.node(block).clone() else {
        unreachable!()
    };
    let mut output = Vec::with_capacity(statements.len());
    for statement in statements {
        let statement = transform_node(ast, res, analyzer, statement, kind, lowered);
        let start = ast.nodes.len();
        let candidate = match kind {
            ConditionalKind::Calls => call_candidate(ast, res, analyzer, statement),
            ConditionalKind::Assignments => assignment_candidate(ast, analyzer, statement),
        };
        let transformation = match kind {
            ConditionalKind::Calls => "conditional-call-lowering",
            ConditionalKind::Assignments => "conditional-assignment-lowering",
        };
        attribute_conditional_nodes(ast, start, statement, None, transformation);
        let selected = if let Some(candidate) = candidate {
            let old_len = measure_stmt(ast, statement);
            let new_len = measure_stmt(ast, candidate);
            if new_len < old_len {
                *lowered += 1;
                candidate
            } else {
                statement
            }
        } else {
            statement
        };
        output.push(selected);
    }
    let statements = output;
    ast.nodes
        .rewrite(block, Node::Block(statements), "conditional-lowering");
    block
}

fn process(ast: &mut Ast, root: NodeId, kind: ConditionalKind) -> PassResult {
    let original = measure_size(ast, root);
    let source_nodes = ast.nodes.clone();
    let mut source = storm_lua_syntax::ast_utils::inherit_ast(ast);
    source.nodes = source_nodes;
    let res = resolve(&source, root);
    let analyzer = EffectAnalyzer::new(&source, &res, root, true);
    let mut lowered = 0usize;
    let out = transform_node(ast, &res, &analyzer, root, &kind, &mut lowered);
    let after = measure_size(ast, out);
    PassResult {
        root: out,
        saved: Some(original.saturating_sub(after) as u64),
        details: Some(if lowered == 0 {
            Vec::new()
        } else {
            vec![format!("lowered={lowered}")]
        }),
    }
}

pub fn lower_conditional_calls(ast: &mut Ast, root: NodeId) -> PassResult {
    process(ast, root, ConditionalKind::Calls)
}

pub fn lower_conditional_assignments(ast: &mut Ast, root: NodeId) -> PassResult {
    process(ast, root, ConditionalKind::Assignments)
}

pub fn lower_conditional_returns(ast: &mut Ast, root: NodeId) -> PassResult {
    process_returns(ast, root)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use storm_lua_syntax::parser::parse_source;
    use storm_lua_syntax::print::Printer;

    fn output(source: &str, calls: bool) -> String {
        let (mut ast, root) = parse_source(source).expect("parse");
        let result = if calls {
            lower_conditional_calls(&mut ast, root)
        } else {
            lower_conditional_assignments(&mut ast, root)
        };
        Printer::new(&ast, false).output(result.root)
    }

    fn return_output(source: &str) -> String {
        let (mut ast, root) = parse_source(source).expect("parse");
        let result = lower_conditional_returns(&mut ast, root);
        Printer::new(&ast, false).output(result.root)
    }

    #[test]
    fn lowers_same_call_with_one_truthy_argument() {
        let out = output(
            "if x then output.setNumber(1,2) else output.setNumber(1,3) end",
            true,
        );
        assert_eq!(out, "output.setNumber(1,x and 2 or 3)");
    }

    #[test]
    fn preserves_effectful_conditions() {
        let source = "if f() then output.setNumber(1,2) else output.setNumber(1,3) end";
        let (ast, root) = parse_source(source).expect("parse");
        let expected = Printer::new(&ast, false).output(root);
        assert_eq!(output(source, true), expected);
    }

    #[test]
    fn lowers_assignment_with_self_fallback() {
        assert_eq!(output("if c then x=1 end", false), "x=c and 1 or x");
    }

    #[test]
    fn rejects_false_yes_value() {
        let out = output("if c then x=false else x=true end", false);
        assert!(out.starts_with("if c then"), "{out}");
    }

    #[test]
    fn lowers_elseif_assignment_chain_to_one_expression() {
        assert_eq!(
            output(
                "if a then x=1 elseif b then x=2 elseif c then x=3 else x=4 end",
                false,
            ),
            "x=a and 1 or b and 2 or c and 3 or 4"
        );
    }

    #[test]
    fn lowers_elseif_chain_with_effectful_conditions_in_order() {
        assert_eq!(
            output("if f() then x=1 elseif g() then x=2 else x=3 end", false,),
            "x=f()and 1 or g()and 2 or 3"
        );
    }

    #[test]
    fn rejects_elseif_chain_with_falsy_nonfinal_value() {
        let source = "if a then x=false elseif b then x=2 else x=3 end";
        let (ast, root) = parse_source(source).expect("parse");
        let expected = Printer::new(&ast, false).output(root);
        assert_eq!(output(source, false), expected);
    }

    #[test]
    fn rejects_elseif_chain_that_assigns_different_targets() {
        let source = "if a then x=1 elseif b then y=2 else x=3 end";
        let (ast, root) = parse_source(source).expect("parse");
        let expected = Printer::new(&ast, false).output(root);
        assert_eq!(output(source, false), expected);
    }

    #[test]
    fn lowers_if_else_returns_with_truthy_true_branch() {
        assert_eq!(
            return_output("if c then return 1 else return 2 end"),
            "return c and 1 or 2"
        );
    }

    #[test]
    fn lowers_guard_return_tail_and_chains() {
        assert_eq!(
            return_output("if a then return 0 end;if b then return 1 end;return-1"),
            "return a and 0 or b and 1 or-1"
        );
    }

    #[test]
    fn uses_inverted_form_when_only_false_branch_is_truthy() {
        assert_eq!(
            return_output("if c then return false else return 1 end"),
            "return not c and 1 or 1>2"
        );
    }

    #[test]
    fn preserves_effectful_condition_evaluation() {
        assert_eq!(
            return_output("if f() then return 1 else return 2 end"),
            "return f()and 1 or 2"
        );
    }

    #[test]
    fn rejects_direct_multivalue_calls_in_either_branch() {
        for source in [
            "if c then return f() else return 1 end",
            "if c then return 1 else return f() end",
            "if c then return 1 end;return f()",
        ] {
            let (ast, root) = parse_source(source).expect("parse");
            let expected = Printer::new(&ast, false).output(root);
            assert_eq!(return_output(source), expected, "{source}");
        }
    }

    #[test]
    fn rejects_multi_expression_returns() {
        let source = "if c then return 1,2 else return 3 end";
        let (ast, root) = parse_source(source).expect("parse");
        let expected = Printer::new(&ast, false).output(root);
        assert_eq!(return_output(source), expected);
    }
}
