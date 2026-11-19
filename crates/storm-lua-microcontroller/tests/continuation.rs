//! Continuation 橋渡しのユニット契約: ヒープ往復、乱数ストリーム、出力保持、HTTPカーソル。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use storm_lua_microcontroller::{Microcontroller, MicrocontrollerConfig};
use storm_lua_spec::io::CompositeSignal;
use storm_lua_vm::runner::{ErrorKind, RunOutcome};

fn unit() -> Microcontroller {
    Microcontroller::new(MicrocontrollerConfig::default()).unwrap()
}

/// Game プロファイルには `assert`/`error` が無い。検証値は tick 中の
/// `output.setNumber` で公開し、ホスト側で読む。
fn probe(vm: &mut Microcontroller) -> CompositeSignal {
    assert_eq!(
        vm.call_tick("probe", &[], &CompositeSignal::default())
            .unwrap(),
        RunOutcome::Completed
    );
    vm.output()
}

const PROBE: &[u8] = b"function probe()\n\
  output.setNumber(1, counter)\n\
  output.setNumber(2, bump(0))\n\
end";

#[test]
fn heap_state_and_closure_upvalues_survive_snapshot_restore() {
    let mut source = unit();
    source
        .load(
            br#"
            counter = 0
            local n = 5
            function bump(d) n = n + d; counter = counter + 1; return n end
            function onTick() bump(1) end
        "#,
            "=unit",
        )
        .unwrap();
    source.load(PROBE, "=probe").unwrap();
    source.tick(&CompositeSignal::default()).unwrap();
    source.tick(&CompositeSignal::default()).unwrap();
    let blob = source.snapshot().unwrap();

    let mut target = unit();
    target.restore(&blob).unwrap();
    // 復元したユニットは capture 時点の続きから実行される。
    assert_eq!(
        target.tick(&CompositeSignal::default()).unwrap(),
        RunOutcome::Completed
    );
    let observed = probe(&mut target);
    // counter: 2 (captured) + 1 (tick) = 3、n: 7 (captured) + 1 = 8。
    assert_eq!(observed.numbers[0], 3.0);
    assert_eq!(observed.numbers[1], 8.0);
    // 元のユニットも独立して同じ地点から続く。
    source.tick(&CompositeSignal::default()).unwrap();
    let original = probe(&mut source);
    assert_eq!(original.numbers[0], observed.numbers[0]);
    assert_eq!(original.numbers[1], observed.numbers[1]);
}

#[test]
fn math_random_sequence_continues_across_snapshot_restore() {
    // 乱数状態はヒープ外なので、continuation は snapshot と一緒に運ぶ。
    let script = br#"
        math.randomseed(20261117)
        function onTick() output.setNumber(1, math.random(1, 1000000)) end
    "#;

    // 無復元の基準列。
    let mut reference = unit();
    reference.load(script, "=reference").unwrap();
    reference.tick(&CompositeSignal::default()).unwrap();
    let first = reference.output().numbers[0];
    reference.tick(&CompositeSignal::default()).unwrap();
    let second = reference.output().numbers[0];
    reference.tick(&CompositeSignal::default()).unwrap();
    let third = reference.output().numbers[0];

    let mut source = unit();
    source.load(script, "=source").unwrap();
    source.tick(&CompositeSignal::default()).unwrap();
    assert_eq!(source.output().numbers[0], first);
    let stream = source.random_stream_state().unwrap();
    let blob = source.snapshot().unwrap();

    let mut target = unit();
    target.restore(&blob).unwrap();
    target.set_random_stream_state(stream).unwrap();
    target.tick(&CompositeSignal::default()).unwrap();
    assert_eq!(
        target.output().numbers[0],
        second,
        "restored unit continued the stream instead of replaying it"
    );
    // 以降も同じ列が続く。
    target.tick(&CompositeSignal::default()).unwrap();
    assert_eq!(target.output().numbers[0], third);
    assert_ne!(third, second);
}

#[test]
fn output_accumulator_is_held_until_explicitly_overwritten() {
    let mut vm = unit();
    vm.load(
        br#"
        local first = true
        function onTick()
            if first then output.setNumber(1, 42); first = false end
        end
    "#,
        "=hold",
    )
    .unwrap();
    vm.tick(&CompositeSignal::default()).unwrap();
    assert_eq!(vm.output().numbers[0], 42.0);
    // 何も書かないtickでも値は保持される。
    vm.tick(&CompositeSignal::default()).unwrap();
    assert_eq!(vm.output().numbers[0], 42.0);

    let mut carried = CompositeSignal::default();
    carried.numbers[0] = 7.5;
    carried.booleans[3] = true;
    vm.set_output(carried).unwrap();
    assert_eq!(vm.output(), carried);
    // 復元した値は明示的に上書きされるまで保持される。
    vm.tick(&CompositeSignal::default()).unwrap();
    assert_eq!(vm.output(), carried);
}

#[test]
fn http_continuation_cursor_requires_a_quiet_queue() {
    let mut vm = unit();
    vm.load(b"function onTick() end", "=http").unwrap();
    assert_eq!(vm.http_continuation_cursor().unwrap(), 1);
    vm.load(b"async.httpGet(9000, '/pending')", "=queue")
        .unwrap();
    // outbox が空でも、送信待ち・飛行中のどちらも静止状態ではない。
    assert_eq!(
        vm.http_continuation_cursor().unwrap_err().kind,
        ErrorKind::Unsupported
    );
    let request = vm.drain_http_requests().pop().unwrap();
    assert_eq!(
        vm.http_continuation_cursor().unwrap_err().kind,
        ErrorKind::Unsupported
    );
    // 呼び出し側が先に排出（ここでは取り消し）してから取得する。
    vm.cancel_http(request.token).unwrap();
    let cursor = vm.http_continuation_cursor().unwrap();
    assert_eq!(cursor, 2);

    // 新世代で復元すると旧トークンは無効になる。
    vm.restore_http_continuation(cursor).unwrap();
    assert_eq!(vm.http_continuation_cursor().unwrap(), 2);
    assert!(vm.http_reply(request.token, b"stale").is_err());
    // カーソル 0 は復元できない。
    assert_eq!(
        vm.restore_http_continuation(0).unwrap_err().kind,
        ErrorKind::InvalidArgument
    );
}

#[test]
fn failed_unit_propagates_the_typed_snapshot_error() {
    // 通常のLuaエラーは回復可能（VMは失敗しない）ため、失敗状態は
    // メモリ上限超過で作る。
    let mut config = MicrocontrollerConfig::default();
    config.limits.lua_memory_bytes = Some(std::num::NonZeroUsize::new(256 * 1024).unwrap());
    let mut failed = Microcontroller::new(config).unwrap();
    assert!(failed
        .load(
            b"s = '' for i = 1, 100000 do s = s .. '0123456789' end",
            "=failure"
        )
        .is_err());
    assert!(failed.is_failed());
    let error = failed.snapshot().unwrap_err();
    assert_eq!(error.kind, ErrorKind::Failed);
    assert!(failed.restore(&[]).is_err());
    // ヒープ外の状態にも失敗は伝播する。
    assert_eq!(
        failed.random_stream_state().unwrap_err().kind,
        ErrorKind::Failed
    );
}

#[test]
fn bug_t2_02_control_namespace_and_host_bindings_round_trip_across_units() {
    use std::sync::{
        atomic::{AtomicI64, Ordering},
        Arc,
    };
    use storm_lua_spec::environment::EnvironmentProfile;
    use storm_lua_vm::{bindings::HostBindings, value::LuaValue};

    let make_unit = |counter: Arc<AtomicI64>| {
        let mut bindings = HostBindings::default();
        bindings.functions.insert(
            "ext.bump".into(),
            Arc::new(move |_| {
                Ok(vec![LuaValue::Integer(
                    counter.fetch_add(1, Ordering::Relaxed) + 1,
                )])
            }),
        );
        Microcontroller::new(MicrocontrollerConfig {
            environment: EnvironmentProfile::Extended,
            bindings,
            control_namespace: Some("ctrl".into()),
            ..MicrocontrollerConfig::default()
        })
        .unwrap()
    };

    let src_counter = Arc::new(AtomicI64::new(0));
    let mut source = make_unit(Arc::clone(&src_counter));
    source
        .load(
            br#"
            local set_in = ctrl.setInputNumber
            local bump = ext.bump
            function onTick()
                set_in(1, bump())
                output.setNumber(1, input.getNumber(1))
            end
        "#,
            "=ctrl-snap",
        )
        .unwrap();
    source.tick(&CompositeSignal::default()).unwrap();
    assert_eq!(source.output().numbers[0], 1.0);
    let blob = source.snapshot().unwrap();

    let dst_counter = Arc::new(AtomicI64::new(50));
    let mut target = make_unit(Arc::clone(&dst_counter));
    target.restore(&blob).unwrap();
    target.tick(&CompositeSignal::default()).unwrap();
    assert_eq!(target.output().numbers[0], 51.0);
    assert_eq!(src_counter.load(Ordering::Relaxed), 1);
    assert_eq!(dst_counter.load(Ordering::Relaxed), 51);
}

#[test]
fn bug_t2_07_concurrent_read_guards_and_replay_sink_do_not_self_deadlock() {
    use std::{sync::mpsc, time::Duration};
    use storm_lua_spec::{
        draw::{DrawCommand, ScreenError, ScreenSink},
        property::PropertyValue,
    };

    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut cfg = MicrocontrollerConfig::default();
        cfg.properties
            .insert(b"gain".to_vec(), PropertyValue::Number(2.5));
        let mut mc = Microcontroller::new(cfg).unwrap();
        mc.load(
            br#"
            function onTick() output.setNumber(1, property.getNumber("gain") * 4) end
            function onDraw() screen.drawLine(0, 0, 8, 8) end
        "#,
            "=reentrant-readers",
        )
        .unwrap();
        mc.tick(&CompositeSignal::default()).unwrap();
        mc.draw(32, 32).unwrap();

        // Hold Commands and Properties guards simultaneously while reading output/input.
        let cmds = mc.commands();
        let props = mc.properties();
        assert_eq!(cmds.len(), 1);
        assert_eq!(
            props.get(b"gain".as_slice()),
            Some(&PropertyValue::Number(2.5))
        );
        assert_eq!(mc.output().numbers[0], 10.0);
        let _ = mc.input();
        drop(props);
        drop(cmds);

        // Also read mc.output() and mc.properties() from inside ScreenSink::submit during mc.replay().
        struct InspectingSink<'a> {
            mc: &'a Microcontroller,
            seen: usize,
        }
        impl ScreenSink for InspectingSink<'_> {
            fn submit(&mut self, _command: &DrawCommand) -> Result<(), ScreenError> {
                assert_eq!(self.mc.output().numbers[0], 10.0);
                assert_eq!(self.mc.properties().len(), 1);
                self.seen += 1;
                Ok(())
            }
        }
        let mut sink = InspectingSink { mc: &mc, seen: 0 };
        mc.replay(&mut sink).unwrap();
        assert_eq!(sink.seen, 1);
        let _ = tx.send(());
    });

    rx.recv_timeout(Duration::from_millis(1000))
        .expect("BUG-T2-07: Microcontroller self-deadlocked on nested read guards / replay sink");
}

#[test]
fn bug_t2_08_microcontroller_with_require_loader_refuses_snapshot_explicitly() {
    use storm_lua_spec::environment::EnvironmentProfile;
    use storm_lua_vm::source::{RequireLoader, SourceChunk};

    let mc = Microcontroller::new(MicrocontrollerConfig {
        environment: EnvironmentProfile::Extended,
        require_loader: Some(RequireLoader::new(|_| {
            Ok(SourceChunk {
                source: b"x = 1".to_vec(),
                name: "@lib.lua".into(),
            })
        })),
        ..MicrocontrollerConfig::default()
    })
    .unwrap();
    let err = mc.snapshot().unwrap_err();
    assert_eq!(err.kind, ErrorKind::Unsupported);
    assert!(err.message.contains("require"), "{}", err.message);
}
