//! 選択されたLuaバックエンドが実際にWASM上で実行可能であることを証明するためのテスト専用実行可能ファイル。
use std::sync::{
    atomic::{AtomicU32, Ordering},
    Arc,
};
use storm_lua_vm::backend::Lua;

fn check() -> Result<(), Box<dyn std::error::Error>> {
    for _ in 0..4 {
        let lua = Lua::new();
        let captured = Arc::new(AtomicU32::new(0_f32.to_bits()));
        let target = Arc::clone(&captured);
        lua.globals().set(
            "input_number",
            lua.create_function(|_, ()| Ok(16_777_217_f32))?,
        )?;
        lua.globals().set(
            "output_number",
            lua.create_function(move |_, n: f64| {
                target.store((n as f32).to_bits(), Ordering::Relaxed);
                Ok(())
            })?,
        )?;
        let (difference,integer,input):(f64,i64,f64)=lua.load("output_number(16777217.0); return 16777217.0-16777216.0, 9223372036854775807, input_number()").eval()?;
        if difference != 1.0
            || integer != i64::MAX
            || input != 16_777_216.0
            || f32::from_bits(captured.load(Ordering::Relaxed)) != 16_777_216_f32
        {
            return Err("numeric contract mismatch".into());
        }
        if lua.load("local = ???").exec().is_ok() {
            return Err("invalid syntax was accepted".into());
        }
    }
    Ok(())
}

/// 実際のバックエンド実行が成功した場合にのみ0を返します。これは製品APIではありません。
#[allow(unsafe_code)]
#[no_mangle]
pub extern "C" fn sle_test_lua_backend() -> i32 {
    match check() {
        Ok(()) => 0,
        Err(error) => {
            eprintln!("backend smoke failed: {error}");
            1
        }
    }
}
fn main() {}
