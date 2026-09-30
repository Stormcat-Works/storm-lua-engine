use super::*;
use mlua::{Function, Lua};

fn trace(code: &str) -> Vec<u8> {
    let lua = Lua::new();
    let finish: Function = lua
        .load(
            r#"
local out={}
screen={drawText=function(...)
 out[#out+1]='['..select('#',...)..']'
 for i=1,select('#',...)do
  local x=select(i,...) local t=type(x)
  out[#out+1]=t
  if t=='number' then
   out[#out+1]=math.type(x)=='integer' and 'i'..string.pack('<i8',x) or 'f'..string.pack('<d',x)
  elseif t=='string' then out[#out+1]=#x..':'..x
  elseif t=='boolean' then out[#out+1]=tostring(x)end
 end
end}
return function()return table.concat(out)end
"#,
        )
        .eval()
        .unwrap();
    // Execute twice to cover helper-local decoder resets, not just initial use.
    for _ in 0..2 {
        lua.load(code)
            .exec()
            .unwrap_or_else(|e| panic!("{e}\n{code}"));
    }
    finish.call::<mlua::String>(()).unwrap().as_bytes().to_vec()
}
fn atom_source(atom: &Atom) -> String {
    match atom {
        Atom::Num(value) | Atom::Str(value) => value.to_string(),
        Atom::Bool(value) => value.to_string(),
        Atom::Nil => "nil".into(),
        Atom::Neg(value) => format!("(-{})", atom_source(value)),
    }
}
fn verify(shape: &Shape, payload: &Payload, values: &[Vec<Atom>]) {
    use storm_lua_syntax::provenance::GeneratedOrigins;
    let input = values
        .iter()
        .chain(values)
        .map(|args| {
            format!(
                "screen.drawText({}) ",
                args.iter().map(atom_source).collect::<Vec<_>>().join(",")
            )
        })
        .collect::<String>();
    let (source, source_root) =
        storm_lua_syntax::parse_source_with_origins("codec-fixture.lua", &input).unwrap();
    let resolution = resolve(&source, source_root);
    let classifier = Classifier::new(&source, &resolution, source_root);
    let Node::Block(statements) = source.node(source_root) else {
        panic!()
    };
    let calls = statements
        .iter()
        .map(|&stmt| classifier.call(stmt).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(calls.len(), values.len() * 2);
    let mut ast = source.clone();
    let helper = ast.strings.intern("replay");
    let first = ast.nodes.len();
    let mut origins = Some(provenance::Trace::batches(
        &[&calls[..values.len()], &calls[values.len()..]],
        1,
    ));
    let definition = encoding::emit_helper_recording(&mut ast, shape, helper, 0, &mut origins);
    origins
        .take()
        .unwrap()
        .finish(&mut ast, &source, first, definition);
    let mut body = vec![definition];
    for batch in calls.chunks(values.len()) {
        let draw = copy_callee(&source, &mut ast, batch[0].callee);
        let f = name(&mut ast, helper);
        ast.nodes.mark_synthetic(f, "fixture-helper-reference");
        let data = payload.emit(&mut ast);
        provenance::payload(&mut ast, data, &source, batch, shape);
        let call = ast.push(Node::Call(f, vec![draw, data], None));
        let stmt = ast.push(Node::Callstat(call));
        let inputs = batch.iter().map(|c| c.origin_call).collect::<Vec<_>>();
        provenance::derive(&mut ast, call, &source, &inputs, "fixture-replay-call");
        provenance::derive(&mut ast, stmt, &source, &inputs, "fixture-replay-call");
        body.push(stmt);
    }
    let root = ast.push(Node::Block(body));
    ast.nodes
        .derive_from(root, &source.nodes, source_root, "fixture-helper-insertion");
    let printed = storm_lua_syntax::Printer::new(&ast, false).output_with_positions(root);
    let origins = GeneratedOrigins::from_print(&ast, &printed).unwrap();
    origins.validate_for_code(&printed.code).unwrap();
    let gaps = origins
        .mappings
        .iter()
        .filter(|m| m.origin.is_none())
        .map(|m| &printed.code[m.start..m.end])
        .collect::<Vec<_>>();
    assert_eq!(origins.unknown_bytes(), 0, "codec gaps {gaps:?}");
    assert_eq!(trace(&input), trace(&printed.code), "{}", printed.code);
    let decoded: Ast = serde_json::from_slice(&serde_json::to_vec(&ast).unwrap()).unwrap();
    let reprinted = storm_lua_syntax::Printer::new(&decoded, false).output_with_positions(root);
    assert_eq!(
        GeneratedOrigins::from_print(&decoded, &reprinted).unwrap(),
        origins
    );
}

fn numbers(columns: &[Vec<i64>]) -> Vec<Vec<Atom>> {
    (0..columns[0].len())
        .map(|r| {
            columns
                .iter()
                .map(|c| Atom::Num(c[r].to_string().into()))
                .collect()
        })
        .collect()
}
fn shape(codec: Codec, columns: usize) -> Shape {
    Shape {
        targets: vec![0],
        args: vec![(0..columns).map(Arg::Column).collect()],
        columns,
        codec,
        repeats: 1,
        translations: Vec::new(),
    }
}

#[test]
fn grouped_words_replay_complete_groups_and_never_add_padding_calls() {
    let mut exercised = 0;
    for count in [32, 63, 64, 120, 128, 257] {
        for range in [2, 3, 8, 17, 32, 91, 256] {
            let columns = vec![(0..count)
                .map(|i| ((i * 73 + i / 7) % range) as i64 - 51)
                .collect()];
            for (codec, text) in grouped::payloads(&columns, count) {
                let Codec::GroupedBytes { rows, .. } = codec else {
                    unreachable!()
                };
                assert_eq!(count % rows, 0);
                verify(&shape(codec, 1), &Payload::Bytes(text), &numbers(&columns));
                exercised += 1;
            }
        }
    }
    assert!(exercised >= 10);
}
#[test]
fn grouped_model_rejects_overflow_and_nondivisible_tails() {
    assert!(grouped::payloads(&[vec![-1_000_000_001, 1_000_000_001]], 2).is_empty());
    assert!(grouped::payloads(&[vec![0, 1, 0, 1, 0]], 5)
        .iter()
        .all(|(c, _)| matches!(c, Codec::GroupedBytes { rows: 5, .. })));
}
#[test]
fn prefix_records_cover_short_long_boundaries_and_wide_words() {
    for width in [1, 2, 3, 7] {
        for prefixes in [1usize, 35, 90, 91] {
            let capacity = 92i64.pow(width as u32);
            let small = capacity / 92;
            let limit = prefixes as i64 * small + (92 - prefixes as i64) * capacity;
            let values = [
                0,
                prefixes as i64 * small - 1,
                prefixes as i64 * small,
                prefixes as i64 * small + 1,
                limit - 1,
            ];
            let codec = Codec::PrefixBytes {
                biases: vec![-17],
                radices: vec![limit],
                width,
                prefixes,
            };
            let columns = vec![values.into_iter().map(|n| n - 17).collect::<Vec<_>>()];
            let text = shared::fixed_payload(&codec, &columns, values.len()).unwrap();
            verify(&shape(codec, 1), &Payload::Bytes(text), &numbers(&columns));
        }
    }
}
#[test]
fn scalar_palettes_keep_float_bits_nil_strings_booleans_and_argument_count() {
    let palette = [
        Atom::Nil,
        Atom::Bool(false),
        Atom::Bool(true),
        Atom::Num("0.0".into()),
        Atom::Neg(Box::new(Atom::Num("0.0".into()))),
        Atom::Num("1.23456789012345".into()),
        Atom::Str("\"a%\\\\b\"".into()),
    ];
    let values = (0..112)
        .map(|r| {
            vec![
                palette[(r * 17 + r / 3) % palette.len()].clone(),
                Atom::Num("7".into()),
                Atom::Nil,
            ]
        })
        .collect::<Vec<_>>();
    let column = values.iter().map(|r| r[0].clone()).collect::<Vec<_>>();
    let mut base = shape(Codec::Counter, 1);
    base.args = vec![vec![
        Arg::Column(0),
        Arg::Constant(Atom::Num("7".into())),
        Arg::Constant(Atom::Nil),
    ]];
    let proposals = palette::proposals(&base, &[0], &[column], values.len());
    assert!(!proposals.is_empty());
    for (shape, _, payload) in proposals {
        verify(&shape, &payload, &values);
    }
}
#[test]
fn shared_tuple_model_preserves_cross_argument_correlations() {
    let source = (0..4)
        .map(|frame| {
            let calls = (0..40)
                .map(|i| {
                    format!(
                        "screen.drawText({},{},{},{})",
                        i % 31,
                        (i * 13) % 29,
                        if i % 3 == 0 { 16 } else { 2 },
                        if i % 3 == 0 { 1 } else { 8 }
                    )
                })
                .collect::<String>();
            format!("do {calls} end local separator{frame}=1 ")
        })
        .collect::<String>();
    let (mut ast, root) = crate::provenance_audit_support::parse_source(&source).unwrap();
    assert!(shared::synthesize(&mut ast, root, true) > 0);
    let after = crate::provenance_audit_support::Printer::new(&ast, false).output(root);
    assert_eq!(trace(&source), trace(&after));
}
#[test]
fn shared_tuple_planner_refuses_mutation_and_dynamic_arguments() {
    for source in [
        "screen.drawText=function()end ",
        "local _ENV=_ENV ",
        "::label:: ",
    ] {
        let calls = (0..80)
            .map(|i| format!("screen.drawText({},{},1,2)", i % 32, i % 4))
            .collect::<String>();
        let (mut ast, root) =
            crate::provenance_audit_support::parse_source(&format!("{source}{calls}")).unwrap();
        let before = crate::provenance_audit_support::Printer::new(&ast, false).output(root);
        assert_eq!(shared::synthesize(&mut ast, root, true), 0);
        assert_eq!(
            before,
            crate::provenance_audit_support::Printer::new(&ast, false).output(root)
        );
    }
    let source = "function onDraw()screen.drawText(input.getNumber(1),2,3,4)end";
    let (mut ast, root) = crate::provenance_audit_support::parse_source(source).unwrap();
    assert_eq!(shared::synthesize(&mut ast, root, true), 0);
}

#[test]
fn all_scalar_record_encodings_retain_data_origins_and_exact_replay() {
    let columns = vec![
        (0..64).map(|i| i * 13 + (i % 5)).collect::<Vec<i64>>(),
        (0..64).map(|i| 1000 + i / 3).collect(),
    ];
    let atoms = columns
        .iter()
        .map(|c| c.iter().map(|v| Atom::Num(v.to_string().into())).collect())
        .collect::<Vec<_>>();
    let proposals = encode_columns(shape(Codec::Counter, 2), vec![0], atoms, 64, true, true);
    let mut kinds = BTreeSet::new();
    for (shape, _, payload) in proposals {
        kinds.insert(match shape.codec {
            Codec::Table => "table",
            Codec::Rice { .. } => "rice",
            Codec::Delta { .. } => "delta",
            Codec::PackedBytes { .. } => "packed",
            Codec::Bytes(_) => "bytes",
            Codec::GroupedBytes { .. } => "grouped",
            _ => "other",
        });
        verify(&shape, &payload, &numbers(&columns));
    }
    for required in ["table", "rice", "delta", "packed"] {
        assert!(
            kinds.contains(required),
            "missing encoding {required}: {kinds:?}"
        );
    }
    let byte_columns = vec![(0..40).map(|i| (i * 17) % 32 - 9).collect::<Vec<_>>()];
    let bytes = byte_columns[0]
        .iter()
        .map(|n| (*n + 9 + 35) as u8 as char)
        .collect::<String>();
    verify(
        &shape(Codec::Bytes(vec![-9]), 1),
        &Payload::Bytes(bytes),
        &numbers(&byte_columns),
    );
}

#[test]
fn every_draw_portfolio_is_checked_instead_of_only_its_winning_candidate() {
    use storm_lua_syntax::provenance::GeneratedOrigins;
    let sources = [
        (0..72)
            .map(|i| {
                format!(
                    "screen.drawText({},{},{}) ",
                    (i * 7) % 127,
                    (i * 13) % 53,
                    i % 3
                )
            })
            .collect::<String>(),
        (0..72)
            .map(|i| format!("screen.drawText({}, {},'label{}') ", i % 9, i % 6, i % 3))
            .collect(),
        (0..72)
            .map(|i| format!("screen.drawText({},{},{}) ", i, i / 4, 7))
            .collect(),
        (0..5)
            .map(|j| {
                format!(
                    "do {} end local unused{j}=1 ",
                    (0..40)
                        .map(|i| format!(
                            "screen.drawText({},{},{},{}) ",
                            i % 31,
                            (i * 13) % 29,
                            if i % 3 == 0 { 16 } else { 2 },
                            if i % 3 == 0 { 1 } else { 8 }
                        ))
                        .collect::<String>()
                )
            })
            .collect(),
    ];
    let mut transforms = [0usize; 5];
    for source in sources {
        let (original, root) =
            storm_lua_syntax::parse_source_with_origins("portfolio.lua", &source).unwrap();
        let (plain, plain_root) = storm_lua_syntax::parse_source(&source).unwrap();
        for (index, (enhanced, dense)) in [(false, false), (true, false), (true, true)]
            .into_iter()
            .enumerate()
        {
            let (mut a, mut p) = (original.clone(), plain.clone());
            let actual = pack_impl(&mut a, root, true, enhanced, dense);
            let expected = pack_impl(&mut p, plain_root, true, enhanced, dense);
            assert_eq!(actual.saved, expected.saved);
            assert!(a == p);
            transforms[index] += usize::from(actual.saved.unwrap_or(0) > 0);
            let printed =
                storm_lua_syntax::Printer::new(&a, false).output_with_positions(actual.root);
            let origins = GeneratedOrigins::from_print(&a, &printed).unwrap();
            origins.validate_for_code(&printed.code).unwrap();
            assert_eq!(origins.unknown_bytes(), 0, "portfolio {index}");
            assert_eq!(trace(&source), trace(&printed.code));
        }
        for (index, f) in [
            regular::synthesize as fn(&mut Ast, NodeId, bool) -> usize,
            shared::synthesize,
        ]
        .into_iter()
        .enumerate()
        {
            let (mut a, mut p) = (original.clone(), plain.clone());
            let count = f(&mut a, root, true);
            assert_eq!(count, f(&mut p, plain_root, true));
            assert!(a == p);
            transforms[index + 3] += usize::from(count > 0);
            let printed = storm_lua_syntax::Printer::new(&a, false).output_with_positions(root);
            let origins = GeneratedOrigins::from_print(&a, &printed).unwrap();
            origins.validate_for_code(&printed.code).unwrap();
            assert_eq!(origins.unknown_bytes(), 0, "portfolio {}", index + 3);
            assert_eq!(trace(&source), trace(&printed.code));
        }
    }
    assert!(
        transforms.iter().all(|n| *n > 0),
        "unexercised transform {transforms:?}"
    );
}
