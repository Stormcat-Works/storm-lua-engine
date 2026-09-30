//! Attribution for the existing record encoder. Decoder machinery is synthetic;
//! encoded input data and replayed arguments retain their actual contributing
//! expressions. This does not claim character-by-character encoded-byte identity.
use super::*;
use std::ops::Range;

struct Mark {
    nodes: Range<usize>,
    inputs: Vec<NodeId>,
    reason: &'static str,
}

pub(super) struct Trace {
    period: usize,
    calls: Vec<NodeId>,
    callees: Vec<NodeId>,
    arguments: Vec<Vec<(NodeId, Atom)>>,
    marks: Vec<Mark>,
}

pub(super) fn known(source: &Ast, node: NodeId) -> bool {
    let mut complete = true;
    storm_lua_syntax::ast_utils::walk(source, node, &mut |n| {
        complete &= source.nodes.origin(n).is_some();
    });
    complete
}

pub(super) fn derive(
    target: &mut Ast,
    node: NodeId,
    source: &Ast,
    inputs: &[NodeId],
    reason: &str,
) {
    if !target.nodes.tracks_origins() {
        return;
    }
    if inputs.is_empty() || inputs.iter().any(|&n| !known(source, n)) {
        let value = target.node(node).clone();
        target.nodes[node as usize] = value;
        return;
    }
    target
        .nodes
        .derive_from(node, &source.nodes, inputs[0], reason);
    for &input in &inputs[1..] {
        target.nodes.relate_from(node, &source.nodes, input, reason);
    }
}

impl Trace {
    pub(super) fn new(calls: &[&Call], period: usize) -> Self {
        Self {
            period,
            calls: calls.iter().map(|c| c.origin_call).collect(),
            callees: calls.iter().map(|c| c.callee).collect(),
            arguments: calls
                .iter()
                .map(|c| {
                    c.origin_arguments
                        .iter()
                        .copied()
                        .zip(c.args.iter().cloned())
                        .collect()
                })
                .collect(),
            marks: Vec::new(),
        }
    }
    pub(super) fn argument(&mut self, nodes: Range<usize>, slot: usize, column: usize) {
        self.marks.push(Mark {
            nodes,
            inputs: self
                .arguments
                .iter()
                .skip(slot)
                .step_by(self.period)
                .map(|r| r[column].0)
                .collect(),
            reason: "draw-record-argument",
        });
    }
    pub(super) fn replay(
        &mut self,
        function: NodeId,
        call: NodeId,
        statement: NodeId,
        slot: usize,
    ) {
        self.marks.push(Mark {
            nodes: function as usize..function as usize + 1,
            inputs: self
                .callees
                .iter()
                .skip(slot)
                .step_by(self.period)
                .copied()
                .collect(),
            reason: "draw-record-callee",
        });
        let inputs = self
            .calls
            .iter()
            .skip(slot)
            .step_by(self.period)
            .copied()
            .collect::<Vec<_>>();
        for node in [call, statement] {
            self.marks.push(Mark {
                nodes: node as usize..node as usize + 1,
                inputs: inputs.clone(),
                reason: "draw-record-replay",
            });
        }
    }
    pub(super) fn lookup(
        &mut self,
        nodes: Range<usize>,
        shape: &Shape,
        entries: &[Atom],
        atom: Option<&Atom>,
    ) {
        let mut inputs = Vec::new();
        for (slot, args) in shape.args.iter().enumerate() {
            for (column, arg) in args.iter().enumerate() {
                if !matches!(arg,Arg::Lookup(_,values) if values==entries) {
                    continue;
                }
                for row in self.arguments.iter().skip(slot).step_by(self.period) {
                    let (node, value) = &row[column];
                    let matches = atom.is_none_or(|a| {
                        a == value
                            || a.integer()
                                .zip(value.integer())
                                .is_some_and(|(a, b)| a == b)
                    });
                    if matches {
                        inputs.push(*node);
                    }
                }
            }
        }
        self.marks.push(Mark {
            nodes,
            inputs,
            reason: "draw-record-palette-data",
        });
    }
    pub(super) fn finish(self, target: &mut Ast, source: &Ast, first: usize, definition: NodeId) {
        // emit_helper allocates only its own decoder, never imports or mutates
        // source nodes. Apply precise data marks AFTER marking that known shell.
        for node in first..target.nodes.len() {
            target
                .nodes
                .mark_synthetic(node as NodeId, "draw-record-decoder");
        }
        for mark in self.marks {
            let mut inputs = mark.inputs;
            let mut seen = HashSet::new();
            inputs.retain(|n| seen.insert(*n));
            let mut nodes = mark.nodes;
            if let Some(first) = nodes.next() {
                derive(target, first as NodeId, source, &inputs, mark.reason);
                // The same argument formula has the same contributors. Build
                // the merge once, rather than re-walking every original for
                // every generated operator/constant in that formula.
                let snapshot = target.nodes.capture_origin(first as NodeId);
                for node in nodes {
                    let value = target.node(node as NodeId).clone();
                    target.nodes[node] = value;
                    target
                        .nodes
                        .finish_rewrite(node as NodeId, snapshot.clone(), mark.reason);
                }
            }
        }
        for original in self.calls {
            target
                .nodes
                .relate_from(definition, &source.nodes, original, "draw-record-decoder");
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use storm_lua_syntax::provenance::{GeneratedOrigins, OriginKind};
    use storm_lua_syntax::{parse_source_with_origins, Printer};

    fn source() -> String {
        format!(
            "function onDraw(){}end",
            (0..96)
                .map(|i| format!(
                    "screen.drawRectF({},{},{},{}) ",
                    (i * 7) % 127,
                    (i * 11) % 61,
                    1 + i % 3,
                    2 + i % 2
                ))
                .collect::<String>()
        )
    }
    fn print(ast: &Ast, root: NodeId) -> (String, GeneratedOrigins) {
        let p = Printer::new(ast, false).output_with_positions(root);
        let origins = GeneratedOrigins::from_print(ast, &p).unwrap();
        origins.validate_for_code(&p.code).unwrap();
        (p.code, origins)
    }
    fn replay(source: &str) -> Vec<Vec<f64>> {
        let lua = mlua::Lua::new();
        lua.load("records={}screen={drawRectF=function(...)records[#records+1]={...}end}")
            .exec()
            .unwrap();
        lua.load(source).exec().unwrap();
        lua.globals()
            .get::<mlua::Function>("onDraw")
            .unwrap()
            .call::<()>(())
            .unwrap();
        let rows = lua.globals().get::<mlua::Table>("records").unwrap();
        rows.sequence_values::<mlua::Table>()
            .map(|r| {
                r.unwrap()
                    .sequence_values::<f64>()
                    .collect::<Result<Vec<_>, _>>()
                    .unwrap()
            })
            .collect()
    }

    #[test]
    fn shared_decoder_separates_generated_code_from_encoded_source_values() {
        let source = source();
        let (mut ast, root) = parse_source_with_origins("drawing.lua", &source).unwrap();
        assert!(super::super::shared::synthesize(&mut ast, root, false) > 0);
        let (code, origins) = print(&ast, root);
        assert_eq!(origins.unknown_bytes(), 0);
        let payloads = origins
            .origins
            .iter()
            .filter(|o| o.transformation.as_deref() == Some("draw-record-encoded-payload"))
            .collect::<Vec<_>>();
        assert!(!payloads.is_empty());
        for p in payloads {
            assert_eq!(p.kind, OriginKind::Derived);
            assert!(p.related.len() > 30);
        }
        assert!(origins
            .origins
            .iter()
            .any(|o| o.kind == OriginKind::Synthetic
                && o.transformation.as_deref() == Some("draw-record-decoder")));
        assert!(origins.origins.iter().any(|o| o.transformation.as_deref()
            == Some("draw-record-palette-data")
            && o.primary.is_some()));
        assert!(origins
            .origins
            .iter()
            .filter(|o| o.kind == OriginKind::Synthetic)
            .all(|o| o.primary.is_none()));
        assert_eq!(replay(&source), replay(&code));
    }

    #[test]
    fn missing_record_argument_stays_unknown_while_decoder_scaffolding_is_known() {
        let source = source();
        let (mut ast, root) = parse_source_with_origins("missing.lua", &source).unwrap();
        let id = ast
            .nodes
            .iter()
            .position(|n| matches!(n, Node::Num(_)))
            .unwrap();
        let old = ast.nodes[id].clone();
        ast.nodes[id] = old;
        assert!(super::super::shared::synthesize(&mut ast, root, false) > 0);
        let (code, origins) = print(&ast, root);
        assert!(origins.unknown_bytes() > 0);
        let printed = Printer::new(&ast, false).output_with_positions(root);
        assert!(printed
            .emissions
            .iter()
            .any(|e| matches!(ast.node(e.node), Node::Str(_))
                && ast.nodes.origin(e.node).is_none()
                && origins
                    .mappings
                    .iter()
                    .any(|m| m.origin.is_none() && m.start < e.end && m.end > e.start)));
        assert!(origins
            .origins
            .iter()
            .any(|o| o.kind == OriginKind::Synthetic));
        assert_eq!(replay(&source), replay(&code));
    }

    #[test]
    fn source_definition_constants_survive_forwarding_wrapper_normalization() {
        let source=format!("function rect(x,y,w,h)screen.drawRectF(x,y,w,h)end function narrow(x,y,w)rect(x,y,w,2)end function onDraw(){}end",(0..96).map(|i|format!("narrow({},{},{}) ",(i*7)%127,(i*11)%61,1+i%3)).collect::<String>());
        let (mut ast, root) = parse_source_with_origins("wrapper.lua", &source).unwrap();
        assert!(super::super::shared::synthesize(&mut ast, root, false) > 0);
        let (code, origins) = print(&ast, root);
        assert_eq!(origins.unknown_bytes(), 0);
        let fixed = source.find("w,2)").unwrap() + 2;
        assert!(origins
            .origins
            .iter()
            .any(|o| o.primary.is_some_and(|s| s.start == fixed)
                || o.related.iter().any(|s| s.start == fixed)));
        assert_eq!(replay(&source), replay(&code));
    }

    #[test]
    fn helper_source_map_roundtrip_does_not_turn_synthetic_into_source() {
        let source = source();
        let (mut ast, root) = parse_source_with_origins("data.lua", &source).unwrap();
        assert!(super::super::shared::synthesize(&mut ast, root, false) > 0);
        let (code, origins) = print(&ast, root);
        let decoded: GeneratedOrigins =
            serde_json::from_slice(&serde_json::to_vec(&origins).unwrap()).unwrap();
        decoded.validate_for_code(&code).unwrap();
        assert_eq!(decoded, origins);
    }

    #[test]
    fn tracing_does_not_change_shared_decoder_syntax_or_its_selected_representation() {
        let source = source();
        let (mut tracked, root) = parse_source_with_origins("code.lua", &source).unwrap();
        let (mut plain, plain_root) = storm_lua_syntax::parse_source(&source).unwrap();
        assert_eq!(
            super::super::shared::synthesize(&mut tracked, root, false),
            super::super::shared::synthesize(&mut plain, plain_root, false)
        );
        assert!(tracked == plain);
        assert_eq!(root, plain_root);
        assert_eq!(
            Printer::new(&tracked, true).output(root),
            Printer::new(&plain, true).output(plain_root)
        );
    }
}
