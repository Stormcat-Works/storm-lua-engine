//! 呼べないphaseでのinput/output/screenは、実ゲームと同じ文言のLuaエラーになる。
//! 根拠はdocs-internalのLua解析（vehicle/io-contract、vehicle/screen、vehicle/lifecycle）。
use std::error::Error;
use storm_lua_microcontroller::{Microcontroller, MicrocontrollerConfig};
use storm_lua_spec::{environment::EnvironmentProfile, io::CompositeSignal};
use storm_lua_vm::runner::{ErrorKind, RunOutcome};

type TestResult = Result<(), Box<dyn Error>>;

const INPUT: &str = "attempting to use input function outside onTick";
const OUTPUT: &str = "attempting to use output function outside onTick";
const SCREEN: &str = "attempting to use screen function outside onDraw";

fn vehicle(
    environment: EnvironmentProfile,
    source: &str,
) -> Result<Microcontroller, Box<dyn Error>> {
    let mut vm = Microcontroller::new(MicrocontrollerConfig {
        environment,
        ..Default::default()
    })?;
    vm.load(source.as_bytes(), "@main.lua")?;
    Ok(vm)
}
fn signal() -> CompositeSignal {
    let mut signal = CompositeSignal::default();
    signal.numbers[0] = 5.0;
    signal.booleans[0] = true;
    signal
}
const PROFILES: [EnvironmentProfile; 2] = [EnvironmentProfile::Game, EnvironmentProfile::Extended];

#[test]
fn input_and_output_inside_on_draw_are_lua_errors_with_the_game_wording() -> TestResult {
    for environment in PROFILES {
        for (call, wording) in [
            ("input.getNumber(1)", INPUT),
            ("input.getBool(1)", INPUT),
            ("output.setNumber(1, 123)", OUTPUT),
            ("output.setBool(1, true)", OUTPUT),
        ] {
            let source = format!("function onTick()\n  output.setNumber(2, 7)\nend\nfunction onDraw()\n  local x = 1\n  {call}\nend");
            let mut vm = vehicle(environment, &source)?;
            vm.tick(&signal())?;
            let error = vm.draw(32, 32).err().ok_or(call)?;
            assert_eq!(error.kind, ErrorKind::Lua, "{call}");
            // ゲームと同じ文言に、呼び出した行が付く。
            assert!(
                error.message.contains(&format!("main.lua:6: {wording}")),
                "{call}: {}",
                error.message
            );
            // スクリプトは止まり、書き込みは起きていない。VMは失敗状態にはならず、
            // onDraw失敗としてステータス1が記録される（E9）。
            assert!(!vm.is_failed());
            assert_eq!(vm.error_status(), 1, "{call}");
            assert_eq!(vm.output().numbers[0], 0.0);
            assert!(!vm.output().booleans[0]);
            assert_eq!(vm.output().numbers[1], 7.0);
        }
    }
    Ok(())
}

#[test]
fn screen_inside_on_tick_is_a_lua_error_with_the_game_wording() -> TestResult {
    for environment in PROFILES {
        for call in [
            "screen.getWidth()",
            "screen.getHeight()",
            "screen.setColor(255, 0, 0)",
            "screen.drawRectF(40, 60, 20, 20)",
            "screen.drawClear()",
            "screen.drawText(0, 0, 'x')",
            "screen.drawMap(0, 0, 1)",
            "screen.setMapColorOcean(0, 0, 0)",
        ] {
            let source = format!(
                "function onTick()\n  {call}\nend\nfunction onDraw()\n  screen.drawClear()\nend"
            );
            let mut vm = vehicle(environment, &source)?;
            assert_eq!(vm.draw(32, 32)?, RunOutcome::Completed);
            let error = vm.tick(&signal()).err().ok_or(call)?;
            assert_eq!(error.kind, ErrorKind::Lua, "{call}");
            assert!(
                error.message.contains(&format!("main.lua:2: {SCREEN}")),
                "{call}: {}",
                error.message
            );
            assert!(!vm.is_failed());
            assert_eq!(vm.error_status(), 2, "{call}");
        }
    }
    Ok(())
}

#[test]
fn a_function_captured_at_top_level_is_checked_when_it_is_called() -> TestResult {
    // ゲームは表を消すのではなく、関数が呼ばれた段階を調べる。
    let mut vm = vehicle(
        EnvironmentProfile::Game,
        "local S, read = screen, input.getNumber\nfunction onTick()\n  S.drawRectF(0, 0, 1, 1)\nend\nfunction onDraw()\n  read(1)\nend",
    )?;
    let error = vm.draw(32, 32).err().ok_or("captured input")?;
    assert!(error.message.contains(&format!("main.lua:6: {INPUT}")));
    vm.reset()?;
    let error = vm.tick(&signal()).err().ok_or("captured screen")?;
    assert!(error.message.contains(&format!("main.lua:3: {SCREEN}")));
    Ok(())
}

#[test]
fn calls_in_their_own_phase_still_work() -> TestResult {
    let mut vm = vehicle(
        EnvironmentProfile::Game,
        "function onTick()\n  output.setNumber(1, input.getNumber(1) * 2)\n  output.setBool(1, input.getBool(1))\nend\nfunction onDraw()\n  screen.setColor(255, 255, 255)\n  screen.drawRectF(0, 0, screen.getWidth(), screen.getHeight())\nend",
    )?;
    vm.tick(&signal())?;
    assert_eq!(vm.output().numbers[0], 10.0);
    assert!(vm.output().booleans[0]);
    vm.draw(32, 16)?;
    assert_eq!(vm.commands().len(), 2);
    Ok(())
}

#[test]
fn top_level_and_http_reply_use_the_same_phase_rules() -> TestResult {
    // トップレベルとhttpReplyでもI/O・描画フラグは0であり、arityが正しければ
    // outsideエラーになります（E4、Phase 1の静的確定）。
    for (call, wording) in [
        ("output.setNumber(1, 9)", OUTPUT),
        ("output.setBool(1, true)", OUTPUT),
        ("local x = input.getNumber(1)", INPUT),
        ("local x = input.getBool(1)", INPUT),
        ("screen.drawClear()", SCREEN),
        ("local w = screen.getWidth()", SCREEN),
    ] {
        let mut vm = Microcontroller::new(MicrocontrollerConfig::default())?;
        let error = vm.load(call.as_bytes(), "@main.lua").err().ok_or(call)?;
        assert_eq!(error.kind, ErrorKind::Lua, "{call}");
        assert!(error.message.contains(wording), "{call}: {}", error.message);
        // ロード失敗はステータス3として記録されます（E9）。
        assert_eq!(vm.error_status(), 3, "{call}");
        assert!(!vm.is_failed());
    }
    // httpReply内でも同じです。
    let mut vm = vehicle(
        EnvironmentProfile::Game,
        "function onTick()\n  async.httpGet(80, '/x')\nend\nfunction httpReply(port, request, reply)\n  output.setNumber(3, 4)\nend",
    )?;
    vm.tick(&signal())?;
    let request = vm.drain_http_requests().pop().ok_or("no request")?;
    let error = vm
        .http_reply(request.token, b"ok")
        .err()
        .ok_or("httpReply output")?;
    assert!(error.message.contains(OUTPUT), "{}", error.message);
    // httpReply失敗はステータス5として記録されます（E9）。
    assert_eq!(vm.error_status(), 5);
    assert_eq!(vm.output().numbers[2], 0.0);
    Ok(())
}

#[test]
fn named_callbacks_use_the_phase_of_their_entry() -> TestResult {
    let mut vm = vehicle(
        EnvironmentProfile::Game,
        "function readInput()\n  return input.getNumber(1)\nend\nfunction paint()\n  screen.drawClear()\nend",
    )?;
    assert_eq!(
        vm.call_tick("readInput", &[], &signal())?,
        RunOutcome::Completed
    );
    assert_eq!(vm.call_draw("paint", &[], 32, 32)?, RunOutcome::Completed);
    let error = vm
        .call_draw("readInput", &[], 32, 32)
        .err()
        .ok_or("input in draw")?;
    assert!(error.message.contains(INPUT));
    vm.reset()?;
    let error = vm
        .call_tick("paint", &[], &signal())
        .err()
        .ok_or("screen in tick")?;
    assert!(error.message.contains(SCREEN));
    Ok(())
}

#[test]
fn phase_is_checked_before_argument_conversion() -> TestResult {
    // 検査順序は arity → phase → 変換 です（E2）。型の合わない引数でも、
    // phase違反が先に報告されます。変換そのものはphase通過後に行われ、
    // 変換できない値はエラーではなく0として扱われます（E1）。
    let mut vm = vehicle(
        EnvironmentProfile::Game,
        "function onDraw()\n  input.getNumber({})\nend",
    )?;
    let error = vm.draw(32, 32).err().ok_or("phase error")?;
    assert_eq!(error.kind, ErrorKind::Lua);
    assert!(error.message.contains(INPUT), "{}", error.message);
    let mut vm = vehicle(
        EnvironmentProfile::Game,
        "function onTick()\n  output.setNumber(1, input.getNumber({}))\n  output.setNumber(2, input.getNumber(true))\nend",
    )?;
    vm.tick(&signal())?;
    assert_eq!(vm.output().numbers[0], 0.0);
    assert_eq!(vm.output().numbers[1], 0.0);
    Ok(())
}

#[test]
fn extended_pcall_can_observe_the_error_message() -> TestResult {
    let mut vm = vehicle(
        EnvironmentProfile::Extended,
        "function onDraw()\n  local ok, message = pcall(input.getNumber, 1)\n  caught = (not ok) and message\nend\nfunction onTick()\n  output.setBool(1, tostring(caught):find('attempting to use input function outside onTick', 1, true) ~= nil)\nend",
    )?;
    vm.draw(32, 32)?;
    vm.tick(&signal())?;
    assert!(vm.output().booleans[0]);
    Ok(())
}
