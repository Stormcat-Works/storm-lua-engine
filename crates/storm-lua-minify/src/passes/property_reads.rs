use crate::config::{PropertyConfig, PropertyMode};
use storm_lua_analysis::effects::EffectAnalyzer;
use storm_lua_analysis::resolver::resolve;
use storm_lua_syntax::ast::{Ast, Node, NodeId};
use storm_lua_syntax::ast_utils::{inherit_ast, map_children};
use storm_lua_syntax::numeric::{decode_lua_string, quote_lua, short_num};

fn rewrite_node(
    source: &Ast,
    target: &mut Ast,
    analyzer: &EffectAnalyzer<'_>,
    node: NodeId,
    config: &PropertyConfig,
    replaced: &mut usize,
) -> NodeId {
    let result = rewrite_node_inner(source, target, analyzer, node, config, replaced);
    if result != node {
        target
            .nodes
            .derive_from(result, &source.nodes, node, "property-hardcoding");
    }
    result
}

fn rewrite_node_inner(
    source: &Ast,
    target: &mut Ast,
    analyzer: &EffectAnalyzer<'_>,
    node: NodeId,
    config: &PropertyConfig,
    replaced: &mut usize,
) -> NodeId {
    let original = source.node(node).clone();
    let (mapped, changed) = map_children(&original, &mut |child| {
        rewrite_node(source, target, analyzer, child, config, replaced)
    });
    if changed {
        target.nodes.rewrite(node, mapped, "property-hardcoding");
    }

    let Node::Call(function, args, _) = original else {
        return node;
    };
    if args.len() != 1 {
        return node;
    }
    let Node::Str(raw_key) = source.node(args[0]) else {
        return node;
    };
    let Some(builtin) = analyzer.resolve_builtin_reference(function) else {
        return node;
    };
    let key = decode_lua_string(raw_key);
    match builtin.as_str() {
        "property.getNumber" => {
            let Some(value) = config.numbers.as_ref().and_then(|values| values.get(&key)) else {
                return node;
            };
            *replaced += 1;
            target.num(short_num(*value))
        }
        "property.getBool" => {
            let Some(value) = config.bools.as_ref().and_then(|values| values.get(&key)) else {
                return node;
            };
            *replaced += 1;
            target.bool_(*value)
        }
        "property.getText" => {
            let Some(value) = config.texts.as_ref().and_then(|values| values.get(&key)) else {
                return node;
            };
            *replaced += 1;
            target.str(quote_lua(value))
        }
        _ => node,
    }
}

pub fn transform_property_reads(
    ast: &Ast,
    root: NodeId,
    config: Option<&PropertyConfig>,
) -> (Ast, NodeId, usize) {
    let Some(config) = config.filter(|value| value.mode == PropertyMode::Hardcode) else {
        return (ast.clone(), root, 0);
    };
    let resolution = resolve(ast, root);
    let analyzer = EffectAnalyzer::new(ast, &resolution, root, true);
    let mut target = inherit_ast(ast);
    target.nodes = ast.nodes.clone();
    let mut replaced = 0usize;
    let target_root = rewrite_node(ast, &mut target, &analyzer, root, config, &mut replaced);
    (target, target_root, replaced)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use storm_lua_syntax::parser::parse_source;
    use storm_lua_syntax::print::Printer;

    fn config() -> PropertyConfig {
        PropertyConfig {
            mode: PropertyMode::Hardcode,
            numbers: Some(BTreeMap::from([("X".to_string(), 12.5)])),
            bools: Some(BTreeMap::from([("Enabled".to_string(), true)])),
            texts: None,
        }
    }

    fn config_with_texts() -> PropertyConfig {
        PropertyConfig {
            texts: Some(BTreeMap::from([("Name".to_string(), "Foo".to_string())])),
            ..config()
        }
    }

    #[test]
    fn hardcodes_known_property_reads() {
        let (ast, root) = parse_source(
            "x=property.getNumber(\"X\") y=property.getBool(\"Enabled\") z=property.getNumber(\"Y\")",
        )
        .unwrap();
        let (out, root, replaced) = transform_property_reads(&ast, root, Some(&config()));
        assert_eq!(replaced, 2);
        assert_eq!(
            Printer::new(&out, false).output(root),
            "x=12.5 y=1>0 z=property.getNumber\"Y\""
        );
    }

    #[test]
    fn hardcodes_known_property_get_text() {
        let (ast, root) =
            parse_source("x=property.getText(\"Name\") y=property.getText(\"Other\")").unwrap();
        let (out, root, replaced) =
            transform_property_reads(&ast, root, Some(&config_with_texts()));
        assert_eq!(replaced, 1);
        assert_eq!(
            Printer::new(&out, false).output(root),
            "x=\"Foo\"y=property.getText\"Other\""
        );
    }

    #[test]
    fn no_text_values_leaves_get_text_untouched() {
        let source = "x=property.getText(\"Name\") y=property.getNumber(\"X\")";
        let (ast, root) = parse_source(source).unwrap();
        let (out, root, replaced) = transform_property_reads(&ast, root, Some(&config()));
        assert_eq!(replaced, 1);
        assert_eq!(
            Printer::new(&out, false).output(root),
            "x=property.getText\"Name\"y=12.5"
        );
    }

    #[test]
    fn shadowed_property_is_not_hardcoded() {
        let (ast, root) = parse_source(
            "local property={getNumber=function()return 9 end} x=property.getNumber(\"X\")",
        )
        .unwrap();
        let (out, root, replaced) = transform_property_reads(&ast, root, Some(&config()));
        assert_eq!(replaced, 0);
        assert!(Printer::new(&out, false)
            .output(root)
            .contains("property.getNumber"));
    }
}
