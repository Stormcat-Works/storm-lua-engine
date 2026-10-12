//! ambient 名前空間の参照規則検出（設計 §4.1）。
//!
//! ルート名 `R`（`ambient` のキー。runner では `sim`）は
//! **静的メンバーアクセスの起点としてのみ**出現できる:
//!
//! - `R.name` : 正当。`members` に存在すれば使用として記録する（tree shaking・閉包解決の入力）。
//!   存在しなければ `unknown-ambient-member`、`kind: environmentOnly` なら `environment-only-api`。
//! - `R` 単体を値として使う（`local s = R` 等）→ `ambient-root-escapes`
//! - `R[expr]` の動的アクセス → `ambient-dynamic-access`
//! - `R` または `R.name` への代入 → `ambient-assigned`（ambient は読み取り専用）
//!
//! `require_scan.rs` とは異なり、ambient はどの深さのスコープからでも参照され得る
//! 通常のグローバルであるため、トップレベル限定ではなくモジュール全体
//! （ネストしたブロック・関数本体を含む）を走査する。この走査ロジックは
//! ambient の `source`（`kind: module` メンバー）自身にもそのまま適用する
//! （sibling メンバー参照・違反規則ともにユーザーモジュールと同じ規則が成り立つ — §4.2）。

use std::collections::BTreeMap;

use crate::diagnostic::{codes, Diagnostic, Range};
use crate::project::{AmbientMember, AmbientNamespace};
use storm_lua_syntax::ast::{Ast, Node, NodeId};
use storm_lua_syntax::ast_utils::for_each_child_key;
use storm_lua_syntax::numeric::decode_lua_string;
use storm_lua_syntax::parser::NodePositions;

/// 検出された正当な ambient メンバー使用1件（tree shaking・閉包解決の入力）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AmbientUsage {
    pub root: String,
    pub member: String,
}

struct ScanCtx<'a> {
    ast: &'a Ast,
    positions: &'a NodePositions,
    module: &'a str,
    ambient: &'a BTreeMap<String, AmbientNamespace>,
    usages: Vec<AmbientUsage>,
    diagnostics: Vec<Diagnostic>,
}

impl ScanCtx<'_> {
    fn range_of(&self, id: NodeId) -> Option<Range> {
        self.positions
            .get(id)
            .map(|(line, col)| Range::point(line, col))
    }

    fn push(&mut self, code: &'static str, id: NodeId, message: impl Into<String>) {
        self.diagnostics.push(
            Diagnostic::error(code, message)
                .with_module(self.module.to_string())
                .with_range(self.range_of(id)),
        );
    }
}

/// `id` が ambient ルート名そのものを参照する `Name` ノードなら、そのルート名を返す。
fn root_name_of<'m>(
    ast: &Ast,
    id: NodeId,
    ambient: &'m BTreeMap<String, AmbientNamespace>,
) -> Option<&'m str> {
    let Node::Name(sym) = ast.node(id) else {
        return None;
    };
    let name = ast.strings.get(*sym);
    ambient
        .keys()
        .find(|k| k.as_str() == name)
        .map(|k| k.as_str())
}

/// `R.name` の静的アクセス（`index_id` は Index ノード自身、`key_id` は key 側の Str ノード）を検証する。
fn handle_static_access(ctx: &mut ScanCtx, index_id: NodeId, root: &str, key_id: NodeId) {
    let Node::Str(raw) = ctx.ast.node(key_id) else {
        unreachable!(
            "dot アクセスの key は parser が常に Str ノードとして生成する（構文上の不変条件）"
        );
    };
    let Some(member) = decode_lua_string(raw) else {
        ctx.push(
            codes::UNKNOWN_AMBIENT_MEMBER,
            index_id,
            "ambient member is not UTF-8",
        );
        return;
    };
    #[expect(
        clippy::expect_used,
        reason = "root_name_of returns keys from the same immutable ambient map; the map cannot change during scanning"
    )]
    let namespace = ctx
        .ambient
        .get(root)
        .expect("root_name_of は ambient のキーからのみ root を返す");
    match namespace.members.get(&member) {
        None => {
            ctx.push(
                codes::UNKNOWN_AMBIENT_MEMBER,
                index_id,
                format!("ambient namespace \"{root}\" has no member \"{member}\"."),
            );
        }
        Some(AmbientMember::EnvironmentOnly) => {
            ctx.push(
                codes::ENVIRONMENT_ONLY_API,
                index_id,
                format!(
                    "\"{root}.{member}\" is an environment-only API and cannot be referenced from exportable code."
                ),
            );
        }
        Some(AmbientMember::Module { .. }) => {
            ctx.usages.push(AmbientUsage {
                root: root.to_string(),
                member,
            });
        }
    }
}

fn scan_expr(ctx: &mut ScanCtx, id: NodeId, is_assign_target: bool) {
    match ctx.ast.node(id) {
        Node::Index(obj, key, dot) => {
            let (obj_id, key_id, dot) = (*obj, *key, *dot);
            if let Some(root) = root_name_of(ctx.ast, obj_id, ctx.ambient) {
                let root = root.to_string();
                if dot {
                    if is_assign_target {
                        ctx.push(
                            codes::AMBIENT_ASSIGNED,
                            id,
                            format!(
                                "cannot assign to \"{root}.<member>\": ambient namespace is read-only."
                            ),
                        );
                    } else {
                        handle_static_access(ctx, id, &root, key_id);
                    }
                } else {
                    ctx.push(
                        codes::AMBIENT_DYNAMIC_ACCESS,
                        id,
                        format!(
                            "dynamic member access \"{root}[...]\" is not allowed; use a static \"{root}.name\" reference."
                        ),
                    );
                    scan_expr(ctx, key_id, false);
                }
                return;
            }
            scan_expr(ctx, obj_id, false);
            scan_expr(ctx, key_id, false);
        }
        Node::Name(sym) => {
            let name = ctx.ast.strings.get(*sym).to_string();
            if ctx.ambient.contains_key(&name) {
                if is_assign_target {
                    ctx.push(
                        codes::AMBIENT_ASSIGNED,
                        id,
                        format!("cannot assign to ambient namespace \"{name}\": it is read-only."),
                    );
                } else {
                    ctx.push(
                        codes::AMBIENT_ROOT_ESCAPES,
                        id,
                        format!(
                            "ambient namespace \"{name}\" can only be used as \"{name}.member\"; it cannot be used as a value by itself."
                        ),
                    );
                }
            }
        }
        Node::Assign(vs, es) => {
            let (vs, es) = (vs.clone(), es.clone());
            for v in vs {
                scan_expr(ctx, v, true);
            }
            for e in es {
                scan_expr(ctx, e, false);
            }
        }
        Node::Funcstat(target, fn_) => {
            let (target, fn_) = (*target, *fn_);
            scan_expr(ctx, target, true);
            scan_expr(ctx, fn_, false);
        }
        _ => {
            let mut children = Vec::new();
            for_each_child_key(ctx.ast, id, &mut |_key, child| children.push(child));
            for child in children {
                scan_expr(ctx, child, false);
            }
        }
    }
}

/// モジュール（またはambientの `source`）`module` の参照規則違反を検出する。
/// 戻り値は (正当な使用一覧, 違反診断一覧)。`ambient` が空なら常に空の結果を返す
/// （ambient未定義プロジェクトで走査コストを払わないための早期リターン。P1/P2の既存挙動を変えない）。
pub fn scan_ambient_refs(
    ast: &Ast,
    root: NodeId,
    positions: &NodePositions,
    module: &str,
    ambient: &BTreeMap<String, AmbientNamespace>,
) -> (Vec<AmbientUsage>, Vec<Diagnostic>) {
    if ambient.is_empty() {
        return (Vec::new(), Vec::new());
    }
    let mut ctx = ScanCtx {
        ast,
        positions,
        module,
        ambient,
        usages: Vec::new(),
        diagnostics: Vec::new(),
    };
    scan_expr(&mut ctx, root, false);
    (ctx.usages, ctx.diagnostics)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::project::AmbientMember;
    use storm_lua_syntax::parser::parse_source_with_positions;

    fn sim_ambient() -> BTreeMap<String, AmbientNamespace> {
        let mut members = BTreeMap::new();
        members.insert(
            "clamp".to_string(),
            AmbientMember::Module {
                source: "return function(x, lo, hi) return x end".to_string(),
            },
        );
        members.insert(
            "math".to_string(),
            AmbientMember::Module {
                source: "return {}".to_string(),
            },
        );
        members.insert("setProperty".to_string(), AmbientMember::EnvironmentOnly);
        let mut ambient = BTreeMap::new();
        ambient.insert("sim".to_string(), AmbientNamespace { members });
        ambient
    }

    fn scan(source: &str) -> (Vec<AmbientUsage>, Vec<Diagnostic>) {
        let (ast, root, positions) = parse_source_with_positions(source).expect("parse ok");
        scan_ambient_refs(&ast, root, &positions, "m", &sim_ambient())
    }

    fn codes_of(diagnostics: &[Diagnostic]) -> Vec<&'static str> {
        diagnostics.iter().map(|d| d.code).collect()
    }

    #[test]
    fn static_member_access_is_recorded_as_usage() {
        let (usages, diagnostics) = scan("local clamp = sim.clamp\nreturn clamp\n");
        assert!(diagnostics.is_empty(), "{:?}", codes_of(&diagnostics));
        assert_eq!(
            usages,
            vec![AmbientUsage {
                root: "sim".to_string(),
                member: "clamp".to_string()
            }]
        );
    }

    #[test]
    fn module_member_can_be_used_freely_after_local_binding() {
        let (usages, diagnostics) = scan("local m = sim.math\na = m.lerp(1, 2, 0.5)\n");
        assert!(diagnostics.is_empty(), "{:?}", codes_of(&diagnostics));
        assert_eq!(usages.len(), 1);
    }

    #[test]
    fn unknown_member_is_reported() {
        let (usages, diagnostics) = scan("local x = sim.typo\nreturn x\n");
        assert!(usages.is_empty());
        assert_eq!(codes_of(&diagnostics), vec![codes::UNKNOWN_AMBIENT_MEMBER]);
    }

    #[test]
    fn environment_only_member_is_reported() {
        let (usages, diagnostics) = scan("sim.setProperty(\"x\", 1)\n");
        assert!(usages.is_empty());
        assert_eq!(codes_of(&diagnostics), vec![codes::ENVIRONMENT_ONLY_API]);
    }

    #[test]
    fn root_used_as_bare_value_is_reported() {
        let (usages, diagnostics) = scan("local s = sim\nreturn s\n");
        assert!(usages.is_empty());
        assert_eq!(codes_of(&diagnostics), vec![codes::AMBIENT_ROOT_ESCAPES]);
    }

    #[test]
    fn root_passed_as_argument_is_reported_as_escaping() {
        let (usages, diagnostics) = scan("function f(x) end\nf(sim)\n");
        assert!(usages.is_empty());
        assert_eq!(codes_of(&diagnostics), vec![codes::AMBIENT_ROOT_ESCAPES]);
    }

    #[test]
    fn dynamic_access_is_reported() {
        let (usages, diagnostics) = scan("local k = \"clamp\"\nlocal x = sim[k]\nreturn x\n");
        assert!(usages.is_empty());
        assert_eq!(codes_of(&diagnostics), vec![codes::AMBIENT_DYNAMIC_ACCESS]);
    }

    #[test]
    fn assignment_to_root_is_reported() {
        let (usages, diagnostics) = scan("sim = {}\n");
        assert!(usages.is_empty());
        assert_eq!(codes_of(&diagnostics), vec![codes::AMBIENT_ASSIGNED]);
    }

    #[test]
    fn assignment_to_member_is_reported() {
        let (usages, diagnostics) = scan("sim.clamp = function() end\n");
        assert!(usages.is_empty());
        assert_eq!(codes_of(&diagnostics), vec![codes::AMBIENT_ASSIGNED]);
    }

    #[test]
    fn assignment_via_funcstat_target_is_reported() {
        let (usages, diagnostics) = scan("function sim.clamp() end\n");
        assert!(usages.is_empty());
        assert_eq!(codes_of(&diagnostics), vec![codes::AMBIENT_ASSIGNED]);
    }

    #[test]
    fn reference_inside_nested_function_is_detected() {
        let (usages, diagnostics) =
            scan("function onTick()\n  local c = sim.clamp\n  c(1, 0, 1)\nend\n");
        assert!(diagnostics.is_empty(), "{:?}", codes_of(&diagnostics));
        assert_eq!(usages.len(), 1);
    }

    #[test]
    fn multiple_usages_of_same_member_are_all_recorded() {
        let (usages, diagnostics) = scan("local a = sim.clamp\nlocal b = sim.clamp\nreturn a, b\n");
        assert!(diagnostics.is_empty(), "{:?}", codes_of(&diagnostics));
        assert_eq!(usages.len(), 2);
    }

    #[test]
    fn unrelated_identifiers_are_not_flagged() {
        let (usages, diagnostics) = scan("local sim2 = 1\nreturn sim2\n");
        assert!(usages.is_empty());
        assert!(diagnostics.is_empty(), "{:?}", codes_of(&diagnostics));
    }

    #[test]
    fn empty_ambient_registry_short_circuits() {
        let (ast, root, positions) = parse_source_with_positions("return sim\n").expect("parse ok");
        let (usages, diagnostics) =
            scan_ambient_refs(&ast, root, &positions, "m", &BTreeMap::new());
        assert!(usages.is_empty());
        assert!(diagnostics.is_empty());
    }
}
