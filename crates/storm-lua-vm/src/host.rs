//! Call-lifetime function arguments. Owned LuaValue remains data-only.
use crate::runner::{ErrorKind, HookControl, VmError};
use crate::value::LuaValue;
use mlua::{Lua, MultiValue, Value};
use std::cell::RefCell;

/// Arguments borrowed only during one synchronous host operation. Functions and
/// original tables cannot escape into a checkpoint or an owned host value.
pub struct HostCall<'call> {
    pub(crate) lua: &'call Lua,
    pub(crate) args: &'call MultiValue,
    pub(crate) control: &'call RefCell<HookControl>,
}
/// Host results are either owned data or an unchanged original argument. The
/// latter preserves native passthrough identity without decoding arbitrary tables.
pub enum HostReturn {
    /// Ordinary bounded data results.
    Values(Vec<LuaValue>),
    /// Return one original argument, using a zero-based index.
    Argument(usize),
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
    /// Decode one ordinary data argument with the normal value budget.
    pub fn decode(&self, index: usize) -> Result<LuaValue, VmError> {
        let mut values =
            crate::value::decode(MultiValue::from_vec(vec![self.argument(index)?.clone()]))?;
        Ok(values.remove(0))
    }
    fn check_budget(&self) -> Result<(), VmError> {
        if self.control.borrow().exhausted {
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

#[cfg(feature = "backend-mlua")]
/// Explicit backend adapter for creating a call-lifetime host view. No execution
/// budget is reset by this adapter or by predicate calls.
#[derive(Clone)]
pub struct HostCallContext(std::rc::Rc<RefCell<HookControl>>);
#[cfg(feature = "backend-mlua")]
impl HostCallContext {
    /// Invoke a host operation synchronously and encode its declared result.
    pub fn invoke(
        &self,
        lua: &Lua,
        arguments: MultiValue,
        run: impl FnOnce(HostCall<'_>) -> Result<HostReturn, VmError>,
    ) -> mlua::Result<MultiValue> {
        let output = run(HostCall {
            lua,
            args: &arguments,
            control: &self.0,
        })
        .map_err(mlua::Error::external)?;
        match output {
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
        }
    }
}
#[cfg(feature = "backend-mlua")]
impl crate::runner::Vm {
    /// Obtain the explicit backend adapter before entering Lua. It shares hook
    /// control and can be used while the VM is executing a nested host operation.
    pub fn host_call_context(&self) -> HostCallContext {
        HostCallContext(std::rc::Rc::clone(&self.control))
    }
}
