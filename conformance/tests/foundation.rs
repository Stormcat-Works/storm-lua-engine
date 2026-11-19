//! これらは基盤部分をテストするものであり、未実装のonTick/onDraw動作をテストするものではありません。
use std::error::Error;
use storm_lua_spec::{
    abi,
    io::{Channel, CompositeSignal},
    property::{PropertyBag, PropertyValue},
    screen::Rgba8,
};
use storm_lua_vm::backend::Lua;
use storm_screen_raster::{blend::blend_pixel, font};

#[test]
fn composite_rounds_only_at_the_boundary() -> Result<(), Box<dyn Error>> {
    let channel = Channel::try_from(1)?;
    let mut signal = CompositeSignal::default();
    signal.write_number(channel, 16_777_217.0);
    assert_eq!(signal.read_number(channel), 16_777_216.0);
    assert_eq!(16_777_217.0_f64 - 16_777_216.0_f64, 1.0);
    Ok(())
}
#[test]
fn lua_backend_has_binary64_arithmetic_and_integer64() -> Result<(), Box<dyn Error>> {
    let lua = Lua::new();
    let (difference, kind, integer): (f64, String, i64) = lua
        .load("return 16777217.0-16777216.0, math.type(1), 9223372036854775807")
        .eval()?;
    assert_eq!(difference, 1.0);
    assert_eq!(kind, "integer");
    assert_eq!(integer, i64::MAX);
    Ok(())
}
#[test]
fn lua_callback_conversion_matches_the_wire_contract() -> Result<(), Box<dyn Error>> {
    use std::sync::{
        atomic::{AtomicU32, Ordering},
        Arc,
    };
    let lua = Lua::new();
    let written = Arc::new(AtomicU32::new(0.0_f32.to_bits()));
    let captured = Arc::clone(&written);
    lua.globals()
        .set("get", lua.create_function(|_, ()| Ok(16_777_217.0_f32))?)?;
    lua.globals().set(
        "set",
        lua.create_function(move |_, value: f64| {
            captured.store((value as f32).to_bits(), Ordering::Relaxed);
            Ok(())
        })?,
    )?;
    let (input, internal): (f64, f64) = lua
        .load("set(16777217.0); return get(), 16777217.0")
        .eval()?;
    assert_eq!(input, 16_777_216.0);
    assert_eq!(internal, 16_777_217.0);
    assert_eq!(
        f32::from_bits(written.load(Ordering::Relaxed)),
        16_777_216.0_f32
    );
    Ok(())
}
#[test]
fn channels_reject_invalid_host_access() {
    for c in [0, 33, u32::MAX] {
        assert!(Channel::try_from(c).is_err());
    }
    for c in 1..=32 {
        assert!(Channel::try_from(c).is_ok());
    }
}
#[test]
fn wire_signed_zero_and_nonfinite_values_are_not_silently_normalized() -> Result<(), Box<dyn Error>>
{
    let ch = Channel::try_from(32)?;
    let mut signal = CompositeSignal::default();
    signal.write_number(ch, -0.0);
    assert!(signal.read_number(ch).is_sign_negative());
    signal.write_number(ch, f64::INFINITY);
    assert!(signal.read_number(ch).is_infinite());
    signal.write_number(ch, f64::NAN);
    assert!(signal.read_number(ch).is_nan());
    Ok(())
}
#[test]
fn property_numbers_do_not_inherit_f32_quantization() {
    let mut bag = PropertyBag::default();
    assert!(bag
        .insert(b"Gain".to_vec(), PropertyValue::Number(16_777_217.0))
        .is_none());
    assert_eq!(bag.get(b"Gain"), Some(&PropertyValue::Number(16_777_217.0)));
    assert_eq!(bag.get(b"gain"), None);
}
#[test]
fn property_bytes_and_missing_values_are_distinct() {
    let mut bag = PropertyBag::default();
    let bytes = vec![0, 255, 128];
    bag.insert(b"text".to_vec(), PropertyValue::Text(bytes.clone()));
    assert_eq!(bag.get(b"text"), Some(&PropertyValue::Text(bytes)));
    assert_eq!(bag.remove(b"missing"), None);
    assert_eq!(bag.len(), 1);
    assert!(!bag.is_empty());
}
#[test]
fn alpha_is_source_alpha_not_premultiplied_source_over() {
    let first = blend_pixel(Rgba8([255, 0, 0, 128]), Rgba8([0, 0, 0, 0]));
    assert_eq!(first, Rgba8([128, 0, 0, 64]));
    let second = blend_pixel(Rgba8([255, 0, 0, 128]), first);
    assert_eq!(second, Rgba8([192, 0, 0, 96]));
}
#[test]
fn opaque_replaces_and_transparent_preserves() {
    let d = Rgba8([50, 60, 70, 80]);
    assert_eq!(blend_pixel(Rgba8([1, 2, 3, 255]), d), Rgba8([1, 2, 3, 255]));
    assert_eq!(blend_pixel(Rgba8([1, 2, 3, 0]), d), d);
}
#[test]
fn bundled_font_is_complete_and_has_known_glyphs() {
    assert_eq!(font::GLYPHS.len(), 95);
    for c in 32..=126 {
        assert!(font::ascii_glyph(c).is_some());
    }
    assert_eq!(font::ascii_glyph(65), Some(&[6, 9, 15, 9, 9]));
    assert_eq!(font::ascii_glyph(32), Some(&[0; 5]));
    assert_eq!(font::ascii_glyph(31), None);
    assert_eq!(font::ascii_glyph(127), None);
    assert!(font::GLYPHS.iter().flatten().all(|row| *row < 16));
}
#[test]
fn abi_has_no_implicit_padding_or_f64_signal_fields() {
    assert_eq!(abi::IO_BYTE_LENGTH, 320);
    assert_eq!(abi::IO_ALIGNMENT, 4);
    assert_eq!(
        (
            abi::INPUT_NUMBERS_OFFSET,
            abi::INPUT_BOOLEANS_OFFSET,
            abi::OUTPUT_NUMBERS_OFFSET,
            abi::OUTPUT_BOOLEANS_OFFSET
        ),
        (0, 128, 160, 288)
    );
}
#[test]
fn debug_payload_preserves_integer_and_string_domains() {
    use storm_lua_vm::debug::DebugValue;
    let number = DebugValue::Integer(i64::MAX);
    let bytes = DebugValue::Bytes(vec![0, 255]);
    assert_eq!(number, DebugValue::Integer(9_223_372_036_854_775_807));
    assert_eq!(bytes, DebugValue::Bytes(vec![0, 255]));
}
