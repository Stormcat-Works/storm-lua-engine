//! Canonical optimization pass IDs.
//!
//! 順序は決定性契約の一部。Phase 8 以降はこの Rust 配列が正本で、
//! 最終 TypeScript 配列は migration snapshot test でのみ参照する。

// Emit the public ordered registry and the constant-string membership match
// from the same literals. This avoids scanning all 67 IDs for every runtime
// pass-enable check or metadata field, without maintaining another registry.
macro_rules! optimization_pass_ids {
    ($($index:literal => $id:literal),+ $(,)?) => {
        /// All registered optimization IDs, in their unchanged public order.
        pub const OPTIMIZATION_PASS_IDS:&[&str]=&[$($id),+];
        /// True only for an exact member of the canonical optimization registry.
        pub fn is_valid_pass_id(id:&str)->bool {matches!(id,$($id)|+)}
        /// Stable registry position used by the compact runtime pass mask.
        pub(crate) fn pass_index(id:&str)->Option<usize> {
            match id {
                $($id => Some($index),)+
                _ => None,
            }
        }
    };
}
optimization_pass_ids! {
   0 => "constant-folding",
   1 => "numeric-literal-approximation",
   2 => "binding-unit-rescaling",
   3 => "interval-origin-shifting",
   4 => "integer-loop-call-folding",
   5 => "color-unpack-specialization",
   6 => "constant-argument-specialization",
   7 => "literal-call-folding",
   8 => "split-sign-recomposition-elimination",
   9 => "equivalent-trailing-argument-omission",
   10 => "table-parameter-scalarization",
   11 => "sparse-boolean-decode-scalarization",
   12 => "numeric-boolean-bit-specialization",
   13 => "quotient-remainder-fusion",
   14 => "destructive-radix-helper-synthesis",
   15 => "destructive-radix-terminal-reuse",
   16 => "quotient-chain-fusion",
   17 => "captured-single-use-hoisting",
   18 => "immutable-global-expression-reuse",
   19 => "multiplicative-carrier-reassociation",
   20 => "coefficient-carrier-synthesis",
   21 => "destructive-result-globalization",
   22 => "immutable-carrier-synthesis",
   23 => "callback-exclusive-function-slotting",
   24 => "screen-button-outlining",
   25 => "else-default-hoisting",
   26 => "conditional-call-lowering",
   27 => "conditional-assignment-lowering",
   28 => "write-only-table-field-cleanup",
   29 => "immutable-table-flattening",
   30 => "closed-namespace-scalarization",
   31 => "uniform-table-fill-scalarization",
   32 => "one-use-function-fusion",
   33 => "expression-helper-inlining",
   34 => "tiny-literal-inlining",
   35 => "single-use-local-sinking",
   36 => "dead-local-elimination",
   37 => "terminal-scope-flattening",
   38 => "ordered-screen-call-factoring",
   39 => "constant-wrapper-merging",
   40 => "affine-wrapper-merging",
   41 => "root-local-globalization",
   42 => "function-local-globalization",
   43 => "hybrid-function-local-globalization",
   44 => "global-store-cleanup",
   45 => "single-use-global-forwarding",
   46 => "equal-scratch-value-coalescing",
   47 => "unwritten-global-nil-propagation",
   48 => "temporary-global-packing",
   49 => "output-sequence-loop-synthesis",
   50 => "common-offset-absorption",
   51 => "ordered-screen-loop-synthesis",
   52 => "periodic-screen-loop-synthesis",
   53 => "available-expression-reuse",
   54 => "signed-expression-factoring",
   55 => "one-use-expression-helper-reversal",
   56 => "final-dead-store-elimination",
   57 => "redundant-parentheses-elimination",
   58 => "scope-renaming",
   59 => "api-alias-optimization",
   60 => "closed-table-field-renaming",
   61 => "closed-namespace-devirtualization",
   62 => "exact-numeric-literal-pooling",
   63 => "ordered-draw-record-packing",
   64 => "repeated-draw-sequence-outlining",
   65 => "adjacent-local-declaration-packing",
   66 => "conditional-return-lowering",
}

/// パス ID の型。文字列リテラルを直接使う。
pub type PassId = &'static str;

/// パス個別 ON/OFF のトグル集合。
/// 明示指定された ID のみを `BTreeMap` で保持する（キー存在 = 明示指定）。
/// 未指定の ID は `pass_enabled` により有効になる。未登録IDは無効。
/// exact モードの `EXACT_UNSAFE_PASSES` 無効化は `false` として明示的に含まれる。
pub type PassToggles = std::collections::BTreeMap<&'static str, bool>;

/// `PassRecord.name`（人間可読な表示名）→ 生成元のパス ID の正本対応表。
///
/// `orchestration.rs` の `ap!` / `final_ap!` 呼び出しサイトに埋め込まれた
/// `($id, $name)` リテラルのみから抽出している（`grep -n 'ap!(' -A2` で
/// 抽出元を再確認できる）。呼び出しサイトのリテラルを変更したら、この表も
/// 同じレビュー単位で更新すること（`pass_record_names_cover_all_pass_ids`
/// テストと `crates/stormmin-core/tests` の統合テストが drift を検出する）。
///
/// 同じ id が複数の表示名を持つことがある（例: `constant-folding` は通常
/// ラウンドでは `"constant folding"`、final パスでは
/// `"constant folding (final)"`）。表示名から一意に id へ戻せれば十分なので
/// ここでは `(id, name)` ペアの一覧として持つ。
pub const PASS_RECORD_NAMES: &[(&str, &str)] = &[
    (
        "adjacent-local-declaration-packing",
        "adjacent local declaration packing",
    ),
    (
        "root-local-globalization",
        "post-draw root local globalization",
    ),
    (
        "one-use-function-fusion",
        "post-draw single-use function fusion",
    ),
    (
        "repeated-draw-sequence-outlining",
        "repeated draw sequence outlining",
    ),
    ("ordered-draw-record-packing", "ordered draw record packing"),
    (
        "closed-namespace-devirtualization",
        "closed namespace devirtualization",
    ),
    (
        "exact-numeric-literal-pooling",
        "exact numeric literal pooling",
    ),
    ("closed-table-field-renaming", "closed table field renaming"),
    (
        "unwritten-global-nil-propagation",
        "unwritten global nil propagation",
    ),
    ("constant-folding", "constant folding"),
    (
        "integer-loop-call-folding",
        "integer numeric-for call folding",
    ),
    (
        "color-unpack-specialization",
        "color-unpack helper specialization",
    ),
    (
        "constant-argument-specialization",
        "constant-argument specialization",
    ),
    ("literal-call-folding", "literal user-function call folding"),
    (
        "split-sign-recomposition-elimination",
        "split-sign recomposition elimination",
    ),
    (
        "equivalent-trailing-argument-omission",
        "equivalent trailing argument omission",
    ),
    (
        "table-parameter-scalarization",
        "table-parameter scalarization",
    ),
    (
        "sparse-boolean-decode-scalarization",
        "proof-driven sparse loop-array scalarization",
    ),
    (
        "numeric-boolean-bit-specialization",
        "numeric boolean bit specialization",
    ),
    (
        "quotient-remainder-fusion",
        "quotient/remainder decode fusion",
    ),
    (
        "quotient-chain-fusion",
        "unused-radix quotient chain fusion",
    ),
    (
        "captured-single-use-hoisting",
        "captured single-use hoisting",
    ),
    (
        "immutable-global-expression-reuse",
        "immutable global expression reuse",
    ),
    (
        "multiplicative-carrier-reassociation",
        "multiplicative carrier reassociation",
    ),
    ("write-only-table-field-cleanup", "unread nil field cleanup"),
    (
        "immutable-table-flattening",
        "immutable constant-table flattening",
    ),
    (
        "closed-namespace-scalarization",
        "closed namespace scalar replacement",
    ),
    (
        "one-use-function-fusion",
        "single-use statement/tail function fusion",
    ),
    (
        "expression-helper-inlining",
        "whole-program expression helper inlining",
    ),
    (
        "tiny-literal-inlining",
        "binding-aware tiny literal inlining",
    ),
    (
        "single-use-local-sinking",
        "binding-aware single-use sinking",
    ),
    (
        "signed-expression-factoring",
        "signed repeated expression factoring",
    ),
    ("dead-local-elimination", "dead local elimination"),
    ("terminal-scope-flattening", "terminal scope flattening"),
    (
        "ordered-screen-call-factoring",
        "ordered screen-call factoring",
    ),
    ("binding-unit-rescaling", "numeric binding unit rescaling"),
    ("interval-origin-shifting", "interval origin shifting"),
    (
        "constant-wrapper-merging",
        "translated constant wrapper merging",
    ),
    ("affine-wrapper-merging", "affine constant wrapper merging"),
    ("constant-folding", "constant folding (final)"),
    (
        "numeric-literal-approximation",
        "aggressive numeric literal approximation",
    ),
];

/// `PASS_RECORD_NAMES` の逆引き（表示名 → id）。同じ名前が複数回登場すること
/// はない前提（テストで検証）。
pub fn pass_id_for_record_name(name: &str) -> Option<&'static str> {
    PASS_RECORD_NAMES
        .iter()
        .find(|(_, record_name)| *record_name == name)
        .map(|(id, _)| *id)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn pass_record_names_ids_are_valid() {
        for (id, _name) in PASS_RECORD_NAMES {
            assert!(
                is_valid_pass_id(id),
                "PASS_RECORD_NAMES has unknown pass id: {id}"
            );
        }
    }

    #[test]
    fn pass_record_names_has_no_duplicate_pairs() {
        let mut seen = HashSet::new();
        for pair in PASS_RECORD_NAMES {
            assert!(seen.insert(pair), "duplicate (id, name) pair: {pair:?}");
        }
    }

    #[test]
    fn pass_record_names_have_unique_display_names() {
        let mut names = HashSet::new();
        for (_, name) in PASS_RECORD_NAMES {
            assert!(
                names.insert(*name),
                "duplicate record display name across different ids: {name}"
            );
        }
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod direct_membership_tests {
    use super::*;
    #[test]
    fn membership_matches_ordered_registry_for_exact_names_and_invalid_variants() {
        assert_eq!(OPTIMIZATION_PASS_IDS.len(), 67);
        for (index, &id) in OPTIMIZATION_PASS_IDS.iter().enumerate() {
            assert!(is_valid_pass_id(id));
            assert_eq!(pass_index(id), Some(index));
            for v in [
                String::new(),
                format!("{id}-"),
                format!(" {id}"),
                format!("{id}\0"),
                id.to_uppercase(),
                format!("雪{id}"),
                id[..id.len() - 1].to_owned(),
            ] {
                assert_eq!(
                    is_valid_pass_id(&v),
                    OPTIMIZATION_PASS_IDS.contains(&v.as_str())
                );
            }
            for i in 0..id.len() {
                let mut bytes = id.as_bytes().to_vec();
                bytes[i] = b'_';
                let text = String::from_utf8(bytes).expect("registered IDs are ASCII");
                assert_eq!(
                    is_valid_pass_id(&text),
                    OPTIMIZATION_PASS_IDS.contains(&text.as_str())
                );
            }
        }
        for removed in [
            "general-expression-factoring",
            "repeated-expression-factoring",
            "scalar-vector-loop-synthesis",
            "redundant-nil-fallback-elimination",
        ] {
            assert!(!is_valid_pass_id(removed));
        }
    }
}
