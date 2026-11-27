//! MC APIの引数意味論（E1/E2/E25）とscreen/描画系の意味論（E3/E5/E6/E15/
//! E19/E20）。根拠はdocs-internalのLua解析（vehicle/io-contract、vehicle/screen）。
use std::error::Error;
use storm_lua_microcontroller::{Microcontroller, MicrocontrollerConfig};
use storm_lua_spec::{
    draw::DrawCommand, environment::EnvironmentProfile, io::CompositeSignal, map::MapColorKind,
    screen::Rgba8,
};
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
fn http_get_arity_mismatch_returns_silently_and_queues_nothing() -> TestResult {
    // async.httpGetも他のMC APIと同じく、個数が合わない呼び出しはエラーに
    // ならず戻り値0個で無言に戻り、リクエストは投入されません。
    let mut vm = vehicle(
        EnvironmentProfile::Game,
        "function onTick()\n  async.httpGet()\n  async.httpGet(8080)\n  async.httpGet(8080, '/extra', 'too many')\n  async.httpGet(8080, '/ok')\nend",
    )?;
    vm.tick(&signal())?;
    assert_eq!(vm.error_status(), 0);
    let requests = vm.drain_http_requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].port, 8080);
    assert_eq!(requests[0].request, b"/ok");
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
fn color_conversion_truncates_and_wraps_like_cvttsd2si() -> TestResult {
    // 色成分はゼロ方向へ切り捨てて下位8bitを格納し、クランプしません（E3）。
    // NaN・範囲外は不定値0x80000000→byte 0、非数値は0、alphaの明示nilは0、
    // 省略したときだけ255です。setMapColor*も同じ変換です。
    let mut vm = vehicle(
        EnvironmentProfile::Game,
        "function onDraw()\n  screen.setColor(256, -1, 300.7)\n  screen.setColor(1, 2, 3, 0/0)\n  screen.setColor(1, 2, 3, nil)\n  screen.setColor('x', true, {})\n  screen.setMapColorOcean(-2, 511.9, 1e30, 128)\nend",
    )?;
    vm.draw(32, 32)?;
    let commands = vm.commands();
    assert_eq!(commands[0], DrawCommand::SetColor(Rgba8([0, 255, 44, 255])));
    assert_eq!(commands[1], DrawCommand::SetColor(Rgba8([1, 2, 3, 0])));
    assert_eq!(commands[2], DrawCommand::SetColor(Rgba8([1, 2, 3, 0])));
    assert_eq!(commands[3], DrawCommand::SetColor(Rgba8([0, 0, 0, 255])));
    assert_eq!(
        commands[4],
        DrawCommand::MapColor(MapColorKind::Ocean, Rgba8([254, 255, 0, 128]))
    );
    assert_eq!(vm.error_status(), 0);
    Ok(())
}

#[test]
fn draw_text_box_align_defaults_and_clamps() -> TestResult {
    // alignは省略時-1、明示nilは0、範囲外は[-1, 1]へクランプします（E5）。
    let mut vm = vehicle(
        EnvironmentProfile::Game,
        "function onDraw()\n  screen.drawTextBox(0, 0, 10, 10, 'hi')\n  screen.drawTextBox(0, 0, 10, 10, 'hi', nil)\n  screen.drawTextBox(0, 0, 10, 10, 'hi', 2.7, -2.7)\nend",
    )?;
    vm.draw(32, 32)?;
    let commands = vm.commands();
    assert_eq!(
        commands[0],
        DrawCommand::TextBox([0.0, 0.0, 10.0, 10.0, -1.0, -1.0], b"hi".to_vec())
    );
    assert_eq!(
        commands[1],
        DrawCommand::TextBox([0.0, 0.0, 10.0, 10.0, 0.0, -1.0], b"hi".to_vec())
    );
    assert_eq!(
        commands[2],
        DrawCommand::TextBox([0.0, 0.0, 10.0, 10.0, 1.0, -1.0], b"hi".to_vec())
    );
    assert_eq!(vm.error_status(), 0);
    Ok(())
}

#[test]
fn draw_map_is_limited_to_once_per_frame() -> TestResult {
    // フレーム内の2回目以降は無言で戻り、コマンドは1個だけです（E6）。
    // 非有限座標でもエラーにはなりません。次のフレームでは再び使えます。
    let mut vm = vehicle(
        EnvironmentProfile::Game,
        "function onDraw()\n  screen.drawMap(0, 0, 1)\n  screen.drawMap(0/0, 0, 1)\nend",
    )?;
    vm.draw(32, 32)?;
    assert_eq!(
        vm.commands()
            .iter()
            .filter(|c| matches!(c, DrawCommand::Map(_)))
            .count(),
        1
    );
    assert_eq!(vm.error_status(), 0);
    vm.draw(32, 32)?;
    assert_eq!(
        vm.commands()
            .iter()
            .filter(|c| matches!(c, DrawCommand::Map(_)))
            .count(),
        1
    );
    Ok(())
}

#[test]
fn map_coordinate_conversion_uses_the_spec_formulas() -> TestResult {
    // map.screenToMap / map.mapToScreenは7引数・戻り値2個で、phase検査は
    // ありません（E15）。変換式は仕様のfloat32演算順です。
    let mut vm = vehicle(
        EnvironmentProfile::Extended,
        "function onTick()\n  local x, y = map.screenToMap(1000, 2000, 0.5, 200, 100, 200, 0)\n  output.setNumber(1, x)\n  output.setNumber(2, y)\n  local px, py = map.mapToScreen(1000, 2000, 0.5, 200, 100, x, y)\n  output.setNumber(3, px)\n  output.setNumber(4, py)\n  output.setBool(1, select('#', map.screenToMap(1, 2)) == 0)\n  output.setBool(2, select('#', map.mapToScreen(1, 2, 3, 4, 5, 6)) == 0)\nend",
    )?;
    vm.tick(&signal())?;
    assert_eq!(vm.output().numbers[0], 1250.0);
    assert_eq!(vm.output().numbers[1], 2125.0);
    assert_eq!(vm.output().numbers[2], 200.0);
    assert_eq!(vm.output().numbers[3], 0.0);
    assert!(vm.output().booleans[0]);
    assert!(vm.output().booleans[1]);
    assert_eq!(vm.error_status(), 0);
    Ok(())
}

#[test]
fn screen_size_returns_doubles() -> TestResult {
    // getWidth/getHeightはdoubleで返します（E19）。math.typeで観測できます。
    let mut vm = vehicle(
        EnvironmentProfile::Extended,
        "function onDraw()\n  captured_w = screen.getWidth()\n  captured_h = screen.getHeight()\nend\nfunction onTick()\n  output.setBool(1, math.type(captured_w) == 'float' and math.type(captured_h) == 'float')\n  output.setNumber(1, captured_w)\n  output.setNumber(2, captured_h)\nend",
    )?;
    vm.draw(64, 32)?;
    vm.tick(&signal())?;
    assert!(vm.output().booleans[0]);
    assert_eq!(vm.output().numbers[0], 64.0);
    assert_eq!(vm.output().numbers[1], 32.0);
    Ok(())
}

#[test]
fn reset_preserves_composite_output() -> TestResult {
    // reset（VM再生成）をまたいでもComposite出力キャッシュは保持されます（E20）。
    let mut vm = vehicle(
        EnvironmentProfile::Game,
        "function onTick()\n  output.setNumber(1, 42)\n  output.setBool(2, true)\nend",
    )?;
    vm.tick(&signal())?;
    vm.reset()?;
    assert_eq!(vm.output().numbers[0], 42.0);
    assert!(vm.output().booleans[1]);
    // リセット後も通常どおり動作します。
    vm.tick(&signal())?;
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
