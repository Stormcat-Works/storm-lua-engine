//! Continuation 境界の VM 契約: ヒープ往復、ヒープ外の乱数ストリーム、失敗 VM の拒否。
#![allow(clippy::unwrap_used, clippy::panic)]

use std::sync::Arc;
use storm_lua_vm::{
    random::{CrtRandomSource, SharedCrtRandom},
    runner::{ErrorKind, RandomStreamState, RunOutcome, Vm},
    value::LuaValue,
    ExecutionLimits,
};

fn vm() -> Vm {
    Vm::new(ExecutionLimits::default()).unwrap()
}

/// 式を評価し、結果をグローバル経由で数値として読む。
fn number(target: &mut Vm, expression: &str) -> f64 {
    let source = format!("__probe = ({expression})");
    assert_eq!(
        target.execute(source.as_bytes(), "probe").unwrap(),
        RunOutcome::Completed
    );
    match target.global("__probe").unwrap() {
        LuaValue::Number(value) => value,
        LuaValue::Integer(value) => value as f64,
        other => panic!("expected a number, got {other:?}"),
    }
}

fn boolean(target: &mut Vm, expression: &str) -> bool {
    let source = format!("__probe = ({expression})");
    target.execute(source.as_bytes(), "probe").unwrap();
    target.global("__probe").unwrap() == LuaValue::Bool(true)
}

#[test]
fn heap_data_and_closure_upvalues_round_trip_through_snapshot() {
    let mut source = vm();
    source
        .execute(
            br#"
            shared = {count = 1}
            shared.self = shared
            local n = 5
            function bump(d) n = n + d; return n end
            function read() return n, shared end
        "#,
            "capture",
        )
        .unwrap();
    // 復元前に状態を進めておく（同じ初期状態どうしの比較にしない）。
    assert_eq!(number(&mut source, "bump(3)"), 8.0);
    let blob = source.snapshot().unwrap();

    let mut target = vm();
    target.restore(&blob).unwrap();

    // closure の upvalue セルと素のグローバルグラフが往復する。
    assert_eq!(number(&mut target, "read()"), 8.0);
    assert_eq!(number(&mut target, "bump(4)"), 12.0);
    assert_eq!(number(&mut target, "read()"), 12.0);
    // テーブル同一性と循環も往復する。
    assert!(boolean(
        &mut target,
        "shared.self == shared and shared.count == 1"
    ));
}

#[test]
fn math_random_sequence_continues_across_snapshot_restore() {
    // 無復元のVMで同じ手順を踏み、列が一致することを確かめる。
    let mut reference = vm();
    reference.use_deterministic_random().unwrap();
    reference
        .execute(b"math.randomseed(20261117)", "seed")
        .unwrap();
    let first = number(&mut reference, "math.random(1, 1000000)");

    let mut source = vm();
    source.use_deterministic_random().unwrap();
    source
        .execute(b"math.randomseed(20261117)", "seed")
        .unwrap();
    let before = source.random_stream_state().unwrap();
    let blob = source.snapshot().unwrap();
    let mut target = vm();
    target.use_deterministic_random().unwrap();
    target.restore(&blob).unwrap();
    target.set_random_stream_state(before).unwrap();

    // 復元したVMで続けて引いた列が、無復元のVMの列と一致する。
    assert_eq!(number(&mut source, "math.random(1, 1000000)"), first);
    assert_eq!(number(&mut target, "math.random(1, 1000000)"), first);
    // 続く値も一致する（1 回だけの偶然ではない）。
    let next_reference = number(&mut reference, "math.random(1, 1000000)");
    assert_eq!(
        number(&mut source, "math.random(1, 1000000)"),
        next_reference
    );
    assert_eq!(
        number(&mut target, "math.random(1, 1000000)"),
        next_reference
    );
}

#[test]
fn snapshot_stream_state_is_carried_outside_the_heap() {
    // 乱数状態はヒープに含まれないため、復元しただけでは巻き戻らない。
    // これは設計どおりで、continuation が random_stream_state を運ぶ根拠になる。
    let mut source = vm();
    source.use_deterministic_random().unwrap();
    source
        .execute(b"math.randomseed(20261117)", "seed")
        .unwrap();
    let blob = source.snapshot().unwrap();
    let mut target = vm();
    target.use_deterministic_random().unwrap();
    target.restore(&blob).unwrap();
    assert_eq!(
        target.random_stream_state().unwrap(),
        RandomStreamState::Engine(1),
        "a fresh VM's stream starts at the documented initial value"
    );
    assert_ne!(
        source.random_stream_state().unwrap(),
        RandomStreamState::Engine(1)
    );
    // 明示的に往復させれば以降の列は一致する。
    target
        .set_random_stream_state(source.random_stream_state().unwrap())
        .unwrap();
    assert_eq!(
        number(&mut source, "math.random()"),
        number(&mut target, "math.random()")
    );
}

#[test]
fn failed_vm_refuses_snapshot_with_a_typed_error() {
    // 統合後の意味論では通常のLuaエラーは回復可能でVMは失敗しない。
    // 失敗状態はメモリ上限超過のようなLua以外のエラーで作る。
    let mut failed = Vm::new(ExecutionLimits {
        lua_memory_bytes: Some(std::num::NonZeroUsize::new(256 * 1024).unwrap()),
        ..ExecutionLimits::default()
    })
    .unwrap();
    let outcome = failed.execute(
        b"s = '' for i = 1, 100000 do s = s .. '0123456789' end",
        "failure",
    );
    assert!(outcome.is_err());
    assert!(failed.is_failed());
    let error = failed.snapshot().unwrap_err();
    assert_eq!(error.kind, ErrorKind::Failed);
    // 拒否はヒープにも触らない。
    assert!(failed.restore(&[]).is_err());
}

struct Zeros;
impl CrtRandomSource for Zeros {
    fn draw(&self) -> Result<u32, String> {
        Ok(0)
    }
    fn seed(&self, _: u32) -> Result<(), String> {
        Ok(())
    }
}

#[test]
fn host_owned_random_stream_is_distinguishable_and_rejects_engine_state() {
    let mut target = vm();
    let source: SharedCrtRandom = Arc::new(Zeros);
    target.install_crt_random(source).unwrap();
    assert_eq!(
        target.random_stream_state().unwrap(),
        RandomStreamState::HostOwned
    );
    let error = target
        .set_random_stream_state(RandomStreamState::Engine(7))
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::Unsupported);
    // HostOwned はエンジン所有VMには適用できない。
    let mut engine_owned = vm();
    assert_eq!(
        engine_owned
            .set_random_stream_state(RandomStreamState::HostOwned)
            .unwrap_err()
            .kind,
        ErrorKind::InvalidArgument
    );
}
