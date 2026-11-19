//! Call-lifetime function arguments. Owned LuaValue remains data-only.
use crate::runner::{lock, ErrorKind, HookControl, VmError};
use crate::value::LuaValue;
use mlua::{Lua, MultiValue, Value};
use std::sync::{Arc, Mutex};

/// Arguments borrowed only during one synchronous host operation. Functions and
/// original tables cannot escape into a checkpoint or an owned host value.
pub struct HostCall<'call> {
    pub(crate) lua: &'call Lua,
    pub(crate) args: &'call MultiValue,
    pub(crate) control: &'call Mutex<HookControl>,
}
/// Host results are owned data, an unchanged argument (optionally followed
/// by owned data), or the registered dispatcher itself. Identity returns
/// never cross the owned data boundary.
pub enum HostReturn {
    /// Ordinary bounded data results.
    Values(Vec<LuaValue>),
    /// Return one original argument, using a zero-based index.
    Argument(usize),
    /// Return one original argument, using a zero-based index, followed by
    /// ordinary bounded data results: the mixed identity-plus-value tuple a
    /// native C-stack return produces (for example a retained argument plus
    /// `false`).
    ArgumentAndValues(usize, Vec<LuaValue>),
    /// Return the stable function that dispatched this call. Requires the
    /// explicit backend adapter to supply that function, never a global lookup.
    CalledFunction,
}
impl HostCall<'_> {
    /// Number of original arguments, including trailing nil.
    pub fn len(&self) -> usize {
        self.args.len()
    }
    /// Whether no arguments were passed.
    pub fn is_empty(&self) -> bool {
        self.args.is_empty()
    }
    fn argument(&self, index: usize) -> Result<&Value, VmError> {
        self.args.get(index).ok_or_else(|| {
            VmError::new(
                ErrorKind::InvalidArgument,
                "host argument index out of range",
            )
        })
    }
    /// Check function type without converting it into an owned value.
    pub fn is_function(&self, index: usize) -> Result<bool, VmError> {
        Ok(matches!(self.argument(index)?, Value::Function(_)))
    }
    /// Lua truthiness without invoking code or decoding tables/functions into
    /// owned data. Only nil and boolean false are false, including for cyclic tables.
    pub fn truthy(&self, index: usize) -> Result<bool, VmError> {
        Ok(!matches!(
            self.argument(index)?,
            Value::Nil | Value::Boolean(false)
        ))
    }
    /// Lua's native number/string coercion, preserving string bytes. Other types
    /// return None; they are not formatted through host-language Display.
    pub fn string(&self, index: usize) -> Result<Option<Vec<u8>>, VmError> {
        Ok(self
            .lua
            .coerce_string(self.argument(index)?.clone())?
            .map(|value| value.as_bytes().to_vec()))
    }
    /// Lua's native numeric coercion, including its numeric-string syntax.
    /// Other types return None, matching lua_tonumberx's conversion result.
    pub fn number(&self, index: usize) -> Result<Option<f64>, VmError> {
        Ok(self.lua.coerce_number(self.argument(index)?.clone())?)
    }
    /// Lua's native integer coercion, including integral numeric strings.
    /// Nonintegral numbers and other nonconvertible values return None, matching
    /// lua_tointegerx; the host decides how to handle conversion failure.
    pub fn integer(&self, index: usize) -> Result<Option<i64>, VmError> {
        Ok(self.lua.coerce_integer(self.argument(index)?.clone())?)
    }
    /// Decode one ordinary data argument with the normal value budget.
    pub fn decode(&self, index: usize) -> Result<LuaValue, VmError> {
        let mut values =
            crate::value::decode(MultiValue::from_vec(vec![self.argument(index)?.clone()]))?;
        Ok(values.remove(0))
    }
    fn check_budget(&self) -> Result<(), VmError> {
        if lock(self.control).exhausted {
            Err(VmError::new(
                ErrorKind::Limit,
                "instruction budget exceeded",
            ))
        } else {
            Ok(())
        }
    }
    /// Protected same-VM predicate using the running coroutine's remaining
    /// budget. Only the first result being boolean true selects a candidate.
    /// Ordinary Lua errors and nonboolean results reject that candidate; limits
    /// propagate when reported by the backend. Sticky instruction exhaustion
    /// remains fatal even if a nested Lua pcall catches it. Lua auxiliary buffer
    /// failures reported as RuntimeError follow ordinary protected-call semantics.
    pub fn predicate(&self, index: usize, arguments: &[LuaValue]) -> Result<bool, VmError> {
        self.check_budget()?;
        let Value::Function(function) = self.argument(index)? else {
            return Err(VmError::new(
                ErrorKind::InvalidArgument,
                "predicate argument must be a function",
            ));
        };
        let arguments = crate::value::encode(self.lua, arguments)?;
        let result = function.call::<Value>(arguments);
        self.check_budget()?;
        match result {
            Ok(Value::Boolean(true)) => Ok(true),
            Ok(_) => Ok(false),
            Err(error) => {
                let error = VmError::from(error);
                if error.kind == ErrorKind::Limit {
                    Err(error)
                } else {
                    Ok(false)
                }
            }
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn truthiness_reads_live_values_without_decoding_or_invoking_them() -> Result<(), VmError> {
        let vm = crate::runner::Vm::new(Default::default())?;
        let mut args: MultiValue = vm
            .lua
            .load(
                r#"
            local cyclic={} cyclic.self=cyclic
            return nil,false,true,0,-0.0,0/0,'',cyclic,
                function() error('must not run') end
        "#,
            )
            .eval()?;
        args.push_back(Value::Thread(
            vm.lua
                .create_thread(vm.lua.create_function(|_, ()| Ok(()))?)?,
        ));
        let call = HostCall {
            lua: &vm.lua,
            args: &args,
            control: &vm.control,
        };
        for (index, expected) in [false, false, true, true, true, true, true, true, true, true]
            .into_iter()
            .enumerate()
        {
            assert_eq!(call.truthy(index)?, expected, "argument {index}");
        }
        assert!(call.decode(7).is_err());
        assert!(call.decode(8).is_err());
        assert!(call.truthy(10).is_err());
        Ok(())
    }

    #[cfg(feature = "backend-mlua")]
    #[test]
    fn argument_and_values_returns_the_original_argument_identically() {
        let vm = crate::runner::Vm::new(Default::default()).unwrap();
        let table = vm.lua.load("return {}").eval::<Value>().unwrap();
        let pointer = match &table {
            Value::Table(table) => table.to_pointer(),
            _ => unreachable!(),
        };
        let context = vm.host_call_context();
        let out = context
            .invoke(&vm.lua, MultiValue::from_vec(vec![table]), |call| {
                assert_eq!(call.len(), 1);
                Ok(HostReturn::ArgumentAndValues(
                    0,
                    vec![LuaValue::Bool(false), LuaValue::Integer(7)],
                ))
            })
            .unwrap();
        let out: Vec<Value> = out.into_iter().collect();
        assert_eq!(out.len(), 3);
        match &out[0] {
            Value::Table(table) => assert_eq!(table.to_pointer(), pointer),
            other => panic!("first result must be the original table, got {other:?}"),
        }
        assert_eq!(out[1], Value::Boolean(false));
        assert_eq!(out[2], Value::Integer(7));
        // An out-of-range argument index is an error, never a substitution.
        let context = vm.host_call_context();
        let result = context.invoke(&vm.lua, MultiValue::from_vec(vec![Value::Nil]), |_| {
            Ok(HostReturn::ArgumentAndValues(1, vec![]))
        });
        assert!(result.is_err());
    }
}

#[cfg(feature = "backend-mlua")]
/// Explicit backend adapter for creating a call-lifetime host view. No execution
/// budget is reset by this adapter or by predicate calls.
#[derive(Clone)]
pub struct HostCallContext(Arc<Mutex<HookControl>>);
#[cfg(feature = "backend-mlua")]
impl HostCallContext {
    /// Invoke a host operation synchronously and encode its declared result.
    pub fn invoke(
        &self,
        lua: &Lua,
        arguments: MultiValue,
        run: impl FnOnce(HostCall<'_>) -> Result<HostReturn, VmError>,
    ) -> mlua::Result<MultiValue> {
        self.invoke_inner(lua, arguments, None, run)
    }
    /// Invoke a registered function, permitting a called-function identity result.
    /// The dispatcher supplies its stable public wrapper, including for aliases.
    pub fn invoke_with_function(
        &self,
        lua: &Lua,
        arguments: MultiValue,
        function: &mlua::Function,
        run: impl FnOnce(HostCall<'_>) -> Result<HostReturn, VmError>,
    ) -> mlua::Result<MultiValue> {
        self.invoke_inner(lua, arguments, Some(function), run)
    }
    fn invoke_inner(
        &self,
        lua: &Lua,
        arguments: MultiValue,
        function: Option<&mlua::Function>,
        run: impl FnOnce(HostCall<'_>) -> Result<HostReturn, VmError>,
    ) -> mlua::Result<MultiValue> {
        let output = run(HostCall {
            lua,
            args: &arguments,
            control: &self.0,
        })
        .map_err(mlua::Error::external)?;
        match output {
            HostReturn::CalledFunction => {
                let function = function.ok_or_else(|| {
                    mlua::Error::external(VmError::new(
                        ErrorKind::Host,
                        "called function was not supplied by the dispatcher",
                    ))
                })?;
                Ok(MultiValue::from_vec(vec![Value::Function(
                    function.clone(),
                )]))
            }
            HostReturn::Values(values) => {
                crate::value::encode(lua, &values).map_err(mlua::Error::external)
            }
            HostReturn::Argument(index) => {
                let value = arguments.get(index).ok_or_else(|| {
                    mlua::Error::external(VmError::new(
                        ErrorKind::InvalidArgument,
                        "host return argument index out of range",
                    ))
                })?;
                Ok(MultiValue::from_vec(vec![value.clone()]))
            }
            HostReturn::ArgumentAndValues(index, values) => {
                let value = arguments.get(index).ok_or_else(|| {
                    mlua::Error::external(VmError::new(
                        ErrorKind::InvalidArgument,
                        "host return argument index out of range",
                    ))
                })?;
                let encoded = crate::value::encode(lua, &values).map_err(mlua::Error::external)?;
                let mut out = vec![value.clone()];
                out.extend(encoded);
                Ok(MultiValue::from_vec(out))
            }
        }
    }
}
#[cfg(feature = "backend-mlua")]
impl crate::runner::Vm {
    /// Obtain the explicit backend adapter before entering Lua. It shares hook
    /// control and can be used while the VM is executing a nested host operation.
    pub fn host_call_context(&self) -> HostCallContext {
        HostCallContext(Arc::clone(&self.control))
    }
}

#[cfg(all(test, feature = "backend-mlua"))]
#[allow(clippy::unwrap_used, clippy::panic)]
mod called_function_tests {
    use super::*;
    #[test]
    fn called_function_requires_explicit_dispatcher_and_preserves_identity() {
        let vm = crate::runner::Vm::new(Default::default()).unwrap();
        let adapter = vm.host_call_context();
        let error = adapter
            .invoke(&vm.lua, MultiValue::new(), |_| {
                Ok(HostReturn::CalledFunction)
            })
            .unwrap_err();
        assert!(error
            .to_string()
            .contains("called function was not supplied"));
        let function = vm.lua.create_function(|_, ()| Ok(())).unwrap();
        let result = adapter
            .invoke_with_function(&vm.lua, MultiValue::new(), &function, |_| {
                Ok(HostReturn::CalledFunction)
            })
            .unwrap();
        assert_eq!(result.len(), 1);
        let Value::Function(result) = &result[0] else {
            panic!("expected function")
        };
        assert_eq!(result, &function);
    }
}
