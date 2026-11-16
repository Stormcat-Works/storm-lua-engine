//! MC APIの引数意味論（E1/E2/E25）。根拠はdocs-internalのLua解析
//! （vehicle/io-contract、vehicle/screen）。
use std::error::Error;
use storm_lua_microcontroller::{Microcontroller, MicrocontrollerConfig};
use storm_lua_spec::{environment::EnvironmentProfile, io::CompositeSignal};
use storm_lua_vm::runner::RunOutcome;

type TestResult = Result<(), Box<dyn Error>>;

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
    signal.numbers[1] = 7.0;
    signal.numbers[31] = 32.0;
    signal.booleans[0] = true;
    signal
}

#[test]
fn arity_mismatch_returns_silently_with_no_values() -> TestResult {
    // arityが合わない呼び出しはエラーにならず、戻り値0個で無言に戻ります（E1）。
    let mut vm = vehicle(
        EnvironmentProfile::Extended,
        "function onTick()\n  output.setBool(1, select('#', input.getNumber()) == 0)\n  output.setBool(2, select('#', input.getNumber(1, 2)) == 0)\n  output.setBool(3, select('#', input.getBool(1, 2)) == 0)\n  output.setBool(4, select('#', property.getText()) == 0)\nend",
    )?;
    vm.tick(&signal())?;
    assert!(vm.output().booleans[..4].iter().all(|b| *b));
    assert_eq!(vm.error_status(), 0);
    Ok(())
}

#[test]
fn arity_is_checked_before_the_phase_flag() -> TestResult {
    // output系はarity不一致ならphaseフラグを見ません（E2）。onDraw中の
    // arity違いは無言で、arityが正しい呼び出しだけがoutsideエラーになります。
    let mut vm = vehicle(
        EnvironmentProfile::Game,
        "function onDraw()\n  output.setNumber(1)\n  output.setNumber(1, 2, 3)\n  output.setBool(1)\nend",
    )?;
    assert_eq!(vm.draw(32, 32)?, RunOutcome::Completed);
    assert_eq!(vm.error_status(), 0);
    let mut vm = vehicle(
        EnvironmentProfile::Game,
        "function onDraw()\n  output.setNumber(1, 2)\nend",
    )?;
    assert!(vm.draw(32, 32).is_err());
    assert_eq!(vm.error_status(), 1);
    Ok(())
}

#[test]
fn values_are_coerced_and_failures_become_zero() -> TestResult {
    // 数値文字列は変換され、変換できない値は0になります（E1）。
    // setBoolの値はlua_tobooleanの真偽で解釈されます。
    let mut vm = vehicle(
        EnvironmentProfile::Game,
        "function onTick()\n  output.setNumber(1, input.getNumber('2'))\n  output.setNumber(2, '12.5')\n  output.setBool(1, 'anything')\n  output.setBool(2, nil)\n  output.setNumber(3, input.getNumber(1.9))\nend",
    )?;
    vm.tick(&signal())?;
    assert_eq!(vm.output().numbers[0], 7.0);
    assert_eq!(vm.output().numbers[1], 12.5);
    assert!(vm.output().booleans[0]);
    assert!(!vm.output().booleans[1]);
    assert_eq!(vm.output().numbers[2], 5.0);
    Ok(())
}

#[test]
fn channel_conversion_truncates_and_ignores_out_of_range() -> TestResult {
    // チャンネルはcvttsd2si相当でi32へ切り捨て変換され、1..32以外は無視されます（E25）。
    let mut vm = vehicle(
        EnvironmentProfile::Game,
        "function onTick()\n  output.setNumber(1, input.getNumber(2 ^ 40))\n  output.setNumber(2, input.getNumber(0 / 0))\n  output.setNumber(32.9, 9)\n  output.setNumber(-5, 9)\n  output.setNumber(0, 9)\n  output.setNumber(33, 9)\nend",
    )?;
    vm.tick(&signal())?;
    assert_eq!(vm.output().numbers[0], 0.0);
    assert_eq!(vm.output().numbers[1], 0.0);
    assert_eq!(vm.output().numbers[31], 9.0);
    // 範囲外への書き込みはどこにも現れません。
    assert_eq!(vm.output().numbers.iter().filter(|n| **n == 9.0).count(), 1);
    assert_eq!(vm.error_status(), 0);
    Ok(())
}

#[test]
fn screen_arity_and_conversion_follow_the_same_rules() -> TestResult {
    // screen系もarity不一致は無言です。getWidth/drawClearは個数の検査がありません。
    // 数値変換できない引数は0になります。
    let mut vm = vehicle(
        EnvironmentProfile::Game,
        "function onDraw()\n  screen.drawLine(1, 2, 3)\n  screen.drawText(0, 0)\n  screen.setColor(255, 0)\n  screen.drawClear(1, 2, 3)\n  local w = screen.getWidth(99)\n  screen.drawRectF(true, '20', 5, 5)\nend",
    )?;
    vm.draw(64, 32)?;
    // drawClearとdrawRectFだけがコマンドになります。
    assert_eq!(vm.commands().len(), 2);
    assert_eq!(vm.error_status(), 0);
    Ok(())
}

#[test]
fn property_reads_coerce_the_label_and_default_on_miss() -> TestResult {
    let mut vm = vehicle(
        EnvironmentProfile::Game,
        "function onTick()\n  output.setNumber(1, property.getNumber(123))\n  output.setNumber(2, property.getNumber('missing'))\nend",
    )?;
    vm.tick(&signal())?;
    assert_eq!(vm.output().numbers[0], 0.0);
    assert_eq!(vm.output().numbers[1], 0.0);
    assert_eq!(vm.error_status(), 0);
    Ok(())
}
