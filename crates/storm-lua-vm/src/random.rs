//! Host-owned Windows CRT context shared with non-Lua simulation consumers.
use mlua::{Lua, MultiValue, Table, Value};
use std::rc::Rc;

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
pub type SharedCrtRandom = Rc<dyn CrtRandomSource>;

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
    let seed_source = Rc::clone(&source);
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
    use std::{cell::RefCell, collections::VecDeque};
    #[derive(Default)]
    struct Tape {
        values: RefCell<VecDeque<u32>>,
        seeds: RefCell<Vec<u32>>,
        draws: RefCell<Vec<u32>>,
    }
    impl CrtRandomSource for Tape {
        fn draw(&self) -> Result<u32, String> {
            let value = self
                .values
                .borrow_mut()
                .pop_front()
                .ok_or("random tape exhausted")?;
            self.draws.borrow_mut().push(value);
            Ok(value)
        }
        fn seed(&self, value: u32) -> Result<(), String> {
            self.seeds.borrow_mut().push(value);
            Ok(())
        }
    }
    fn setup(values: &[u32]) -> (Lua, Rc<Tape>) {
        let lua = Lua::new();
        let tape = Rc::new(Tape {
            values: RefCell::new(values.iter().copied().collect()),
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
        assert_eq!(tape.draws.borrow().len(), 10);
        assert!(tape.values.borrow().is_empty());
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
            *tape.seeds.borrow(),
            [1, u32::MAX, 1, 0, 0, 0, 0, 0, u32::MAX - 11, 0]
        );
        assert_eq!(tape.draws.borrow().len(), 10);
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
