//! Call-site locations come from actual Lua execution, not delayed host stack queries.
use storm_lua_addon::{Addon, AddonConfig};
use storm_lua_microcontroller::{Microcontroller, MicrocontrollerConfig};
use storm_lua_spec::environment::EnvironmentProfile;
use storm_lua_vm::{
    logging::{LogLocation, LogRecord, LogSource},
    runner::{ErrorKind, RunOutcome, StepMode},
    source::{RequireLoader, SourceChunk},
};

fn location(chunk: &str, line: u32) -> Option<LogLocation> {
    Some(LogLocation {
        chunk: chunk.into(),
        line,
    })
}

#[test]
fn named_chunks_aliases_wrappers_tick_draw_and_reset_keep_their_actual_lines(
) -> Result<(), Box<dyn std::error::Error>> {
    let mut vm = Microcontroller::new(MicrocontrollerConfig {
        environment: EnvironmentProfile::Extended,
        require_loader: Some(RequireLoader::new(|_| {
            Ok(SourceChunk {
                name: "@lib/ログ.lua".into(),
                source: b"function helper()\n  debug.log('helper')\nend".to_vec(),
            })
        })),
        ..Default::default()
    })?;
    let source = b"local say=print\nprint('top')\nrequire('helper')\nfunction onTick()\n  helper()\n  say('tick')\nend\nfunction onDraw()\n  debug.log('draw')\nend";
    vm.load(source, "@main.lua")?;
    assert_eq!(
        vm.drain_log_records(),
        vec![LogRecord {
            source: LogSource::Print,
            bytes: b"top".to_vec(),
            location: location("@main.lua", 2),
        }]
    );
    for _ in 0..2 {
        vm.tick(&Default::default())?;
        vm.draw(32, 32)?;
        let records = vm.drain_log_records();
        assert_eq!(
            records
                .iter()
                .map(|record| record.location.clone())
                .collect::<Vec<_>>(),
            vec![
                location("@lib/ログ.lua", 2),
                location("@main.lua", 6),
                location("@main.lua", 9),
            ]
        );
        assert_eq!(
            records
                .iter()
                .map(|record| record.bytes.as_slice())
                .collect::<Vec<_>>(),
            vec![b"helper".as_slice(), b"tick", b"draw"]
        );
        vm.reset()?;
        assert_eq!(vm.drain_log_records()[0].location, location("@main.lua", 2));
    }
    Ok(())
}

#[test]
fn logs_before_suspension_and_failure_retain_original_call_sites(
) -> Result<(), Box<dyn std::error::Error>> {
    let mut vm = Microcontroller::new(Default::default())?;
    vm.load(
        b"function onTick()\n  debug.log('before')\n  debug.log('after')\n  missing()\nend",
        "@stop.lua",
    )?;
    vm.set_breakpoints(vec![("@stop.lua".into(), 3)])?;
    assert_eq!(vm.tick(&Default::default())?, RunOutcome::Suspended);
    let before = vm.drain_log_records();
    assert_eq!(before.len(), 1);
    assert_eq!(before[0].location, location("@stop.lua", 2));
    vm.set_breakpoints(vec![])?;
    assert!(vm.resume(StepMode::Continue).is_err());
    let after = vm.drain_log_records();
    assert_eq!(after.len(), 1);
    assert_eq!(after[0].location, location("@stop.lua", 3));
    assert!(vm.drain_log_records().is_empty());
    Ok(())
}

#[test]
fn addon_game_logs_keep_bytes_and_do_not_expose_debug_inspection(
) -> Result<(), Box<dyn std::error::Error>> {
    let mut vm = Addon::new(AddonConfig::default())?;
    vm.load(b"debug.log(type(print),type(debug.getinfo))\nfunction onTick(ticks)\n debug.log(string.char(255),ticks)\nend", "@addon.lua")?;
    let top = vm.drain_log_records();
    assert_eq!(top[0].bytes, b"nil\tnil");
    assert_eq!(top[0].location, location("@addon.lua", 1));
    vm.start()?;
    vm.tick(3)?;
    let records = vm.drain_log_records();
    assert_eq!(records[0].location, location("@addon.lua", 3));
    assert_eq!(records[0].bytes, b"\xff\t3");
    Ok(())
}

#[test]
fn source_metadata_participates_in_the_bounded_log_buffer() -> Result<(), Box<dyn std::error::Error>>
{
    let mut vm = Microcontroller::new(Default::default())?;
    let name = format!("@{}", "x".repeat(1023));
    let result = vm.load(b"for i=1,128 do debug.log('') end", &name);
    assert!(matches!(result, Err(error) if error.kind == ErrorKind::Limit));
    let records = vm.drain_log_records();
    assert_eq!(records.len(), 64);
    assert_eq!(records[0].location, location(&name, 1));
    Ok(())
}
