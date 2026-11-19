//! Host-owned Windows CRT context shared with non-Lua simulation consumers.
use mlua::{Lua, MultiValue, Table, Value};
use std::sync::{
    atomic::{AtomicU32, Ordering},
    Arc,
};

/// One native CRT random context. The host owns/checkpoints its state and shares
/// the same instance with every consumer running in that simulation context.
/// Calls must not re-enter the VM. Values are raw rand() results in 0..=32767.
pub trait CrtRandomSource {
    /// Advance once, including when a subsequent Lua argument check will fail.
    fn draw(&self) -> Result<u32, String>;
    /// Equivalent to srand(seed); no discarded draw is performed by this method.
    fn seed(&self, seed: u32) -> Result<(), String>;
}
/// A shared execution-context resource, retained through addon savedata reload.
pub type SharedCrtRandom = Arc<dyn CrtRandomSource + Send + Sync>;

/// エンジン組み込みのMSVC CRT互換ストリーム。実ゲームが使うCRTの
/// `rand`/`srand`（乗算214013・加算2531011、上位ビットから15bitを取り出す方式）
/// そのもので、初期状態はsrand前の1です。
pub struct MsvcCrtRandom {
    state: AtomicU32,
}
impl MsvcCrtRandom {
    /// 初期状態（1）で作成します。
    pub fn new() -> Self {
        Self {
            state: AtomicU32::new(1),
        }
    }
    /// 現在の内部32bit状態ワードを返します（continuation用）。
    pub fn state(&self) -> u32 {
        self.state.load(Ordering::Relaxed)
    }
    /// 破棄ドローを伴わずに内部32bit状態ワードを直接設定します（continuation用）。
    pub fn set_state(&self, state: u32) {
        self.state.store(state, Ordering::Relaxed);
    }
}
impl Default for MsvcCrtRandom {
    fn default() -> Self {
        Self::new()
    }
}
impl CrtRandomSource for MsvcCrtRandom {
    fn draw(&self) -> Result<u32, String> {
        let mut prev = self.state.load(Ordering::Relaxed);
        loop {
            let next = prev.wrapping_mul(214013).wrapping_add(2531011);
            match self
                .state
                .compare_exchange_weak(prev, next, Ordering::Relaxed, Ordering::Relaxed)
            {
                Ok(_) => return Ok((next >> 16) & 0x7FFF),
                Err(actual) => prev = actual,
            }
        }
    }
    fn seed(&self, seed: u32) -> Result<(), String> {
        self.state.store(seed, Ordering::Relaxed);
        Ok(())
    }
}
pub(crate) fn shared_builtin_crt_arc() -> Arc<MsvcCrtRandom> {
    thread_local! {
        static SHARED: Arc<MsvcCrtRandom> = Arc::new(MsvcCrtRandom::new());
    }
    SHARED.with(Arc::clone)
}
/// プロセス共有の組み込みCRTストリームを返します。実ゲームではCRT乱数は
/// プロセス全体で1本の列を共有するため、Gameプロファイルの既定はこちらです。
/// 構築時の共有単位はホストスレッドごとであり、返されるインスタンス自体は `Send + Sync` です。
pub fn shared_builtin_crt() -> SharedCrtRandom {
    shared_builtin_crt_arc()
}

fn draw(source: &dyn CrtRandomSource) -> mlua::Result<u32> {
    let value = source.draw().map_err(mlua::Error::RuntimeError)?;
    if value > 32767 {
        return Err(mlua::Error::RuntimeError(
            "CRT random provider returned a value above 32767".into(),
        ));
    }
    Ok(value)
}
fn integer(lua: &Lua, args: &MultiValue, index: usize) -> mlua::Result<i64> {
    lua.coerce_integer(args.get(index).cloned().unwrap_or(Value::Nil))?
        .ok_or_else(|| {
            mlua::Error::RuntimeError(format!(
                "bad argument #{} to random (integer expected)",
                index + 1
            ))
        })
}
// CVTTSD2SI returns the indefinite integer (MIN) for NaN/overflow, whereas Rust's
// float cast saturates. Native randomseed converts Lua Number before srand(u32).
fn seed_word(value: f64) -> u32 {
    let integer = if (-9223372036854775808.0..9223372036854775808.0).contains(&value) {
        value as i64
    } else {
        i64::MIN
    };
    integer as u32
}
pub(crate) fn install(lua: &Lua, math: &Table, source: SharedCrtRandom) -> mlua::Result<()> {
    let seed_source = Arc::clone(&source);
    math.raw_set(
        "randomseed",
        lua.create_function(move |lua, args: MultiValue| {
            let value = lua
                .coerce_number(args.front().cloned().unwrap_or(Value::Nil))?
                .ok_or_else(|| {
                    mlua::Error::RuntimeError(
                        "bad argument #1 to randomseed (number expected)".into(),
                    )
                })?;
            seed_source
                .seed(seed_word(value))
                .map_err(mlua::Error::RuntimeError)?;
            draw(seed_source.as_ref())?; // Lua's seed entry discards exactly one draw.
            Ok(())
        })?,
    )?;
    math.raw_set(
        "random",
        lua.create_function(move |lua, args: MultiValue| {
            let r = f64::from(draw(source.as_ref())?) / 32768.;
            match args.len() {
                0 => Ok(Value::Number(r)),
                1 | 2 => {
                    let low = if args.len() == 1 {
                        1
                    } else {
                        integer(lua, &args, 0)?
                    };
                    let high = integer(lua, &args, args.len() - 1)?;
                    if low > high || (low < 0 && high > i64::MAX + low) {
                        return Err(mlua::Error::RuntimeError("invalid random interval".into()));
                    }
                    let offset = (r * ((high - low) as f64 + 1.)) as i64;
                    Ok(Value::Integer(low + offset))
                }
                _ => Err(mlua::Error::RuntimeError(
                    "random expects zero, one or two integers".into(),
                )),
            }
        })?,
    )?;
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use std::{collections::VecDeque, sync::Mutex};
    #[derive(Default)]
    struct Tape {
        values: Mutex<VecDeque<u32>>,
        seeds: Mutex<Vec<u32>>,
        draws: Mutex<Vec<u32>>,
    }
    impl CrtRandomSource for Tape {
        fn draw(&self) -> Result<u32, String> {
            let value = self
                .values
                .lock()
                .map_err(|_| "random tape is poisoned")?
                .pop_front()
                .ok_or("random tape exhausted")?;
            self.draws
                .lock()
                .map_err(|_| "random tape is poisoned")?
                .push(value);
            Ok(value)
        }
        fn seed(&self, value: u32) -> Result<(), String> {
            self.seeds
                .lock()
                .map_err(|_| "random tape is poisoned")?
                .push(value);
            Ok(())
        }
    }
    fn setup(values: &[u32]) -> (Lua, Arc<Tape>) {
        let lua = Lua::new();
        let tape = Arc::new(Tape {
            values: Mutex::new(values.iter().copied().collect()),
            ..Default::default()
        });
        let provider: SharedCrtRandom = tape.clone();
        install(&lua, &lua.globals().get::<Table>("math").unwrap(), provider).unwrap();
        (lua, tape)
    }
    #[test]
    fn sampler_consumes_before_validation_and_uses_native_integer_distribution() {
        let (lua, tape) = setup(&[0, 32767, 0, 32767, 16384, 16384, 1, 1, 1, 1]);
        let output: MultiValue = lua
            .load(
                r#"
          local a=math.random();local b=math.random()
          local c=math.random(10);local d=math.random(-5,5)
          local e=math.random(-9223372036854775808,-1)
          local f=math.random('3','7')
          return a,b,c,d,e,f,
            pcall(math.random,1.25),pcall(math.random,0),
            pcall(math.random,-9223372036854775808,0),pcall(math.random,1,2,3)
        "#,
            )
            .eval()
            .unwrap();
        assert_eq!(output[0], Value::Number(0.));
        assert_eq!(output[1], Value::Number(32767. / 32768.));
        assert_eq!(output[2], Value::Integer(1));
        assert_eq!(output[3], Value::Integer(5));
        assert_eq!(output[4], Value::Integer(-4611686018427387904));
        assert_eq!(output[5], Value::Integer(5));
        // Non-final pcall returns collapse to its first bool. The final one also
        // returns its error string, whose exact wording is outside this contract.
        for i in 6..10 {
            assert_eq!(output[i], Value::Boolean(false));
        }
        assert_eq!(tape.draws.lock().unwrap().len(), 10);
        assert!(tape.values.lock().unwrap().is_empty());
    }
    #[test]
    fn seed_uses_number_conversion_native_indefinite_integer_and_one_discard() {
        let (lua, tape) = setup(&[0; 10]);
        lua.load(
            r#"
          math.randomseed(1.9,'ignored');math.randomseed(-1.9)
          math.randomseed(4294967297);math.randomseed(-0.0)
          math.randomseed(0/0);math.randomseed(1/0)
          math.randomseed(-1/0);math.randomseed(9223372036854775807)
          math.randomseed('-12.75');assert(select('#',math.randomseed(-9223372036854775808))==0)
          local ok=pcall(math.randomseed,{});assert(not ok)
        "#,
        )
        .exec()
        .unwrap();
        assert_eq!(
            *tape.seeds.lock().unwrap(),
            [1, u32::MAX, 1, 0, 0, 0, 0, 0, u32::MAX - 11, 0]
        );
        assert_eq!(tape.draws.lock().unwrap().len(), 10);
    }
    #[test]
    fn builtin_stream_matches_the_msvc_crt_sequence() {
        // MSVC CRT rand の既知の列（srand(1) 後: 41, 18467, 6334, 26500, 19169, ...）。
        let source = MsvcCrtRandom::new();
        // 初期状態はsrand(1)と同じ1のため、seedなしでも同じ列から始まります。
        assert_eq!(source.draw().unwrap(), 41);
        source.seed(1).unwrap();
        let draws: Vec<u32> = (0..5).map(|_| source.draw().unwrap()).collect();
        assert_eq!(draws, [41, 18467, 6334, 26500, 19169]);
        // 共有インスタンスは同一スレッド内で同じ列を返し続けます。
        let shared = shared_builtin_crt();
        shared.seed(7).unwrap();
        let first = shared.draw().unwrap();
        assert_eq!(shared_builtin_crt().draw().unwrap(), {
            let expect = MsvcCrtRandom::new();
            expect.seed(7).unwrap();
            let _ = expect.draw().unwrap();
            let _ = first;
            expect.draw().unwrap()
        });
    }
    #[test]
    fn invalid_provider_output_and_exhausted_source_are_explicit_errors() {
        let (lua, _) = setup(&[32768]);
        let error = lua
            .load("return math.random()")
            .eval::<Value>()
            .unwrap_err();
        assert!(error.to_string().contains("above 32767"));
        let error = lua
            .load("return math.random()")
            .eval::<Value>()
            .unwrap_err();
        assert!(error.to_string().contains("exhausted"));
    }
}
