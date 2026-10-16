//! Borrowed host phases and synchronous callback delivery share one Lua state.
use super::{invalid, Stage};
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};
use storm_lua_vm::{
    backend::{self, Function, Lua, MultiValue, Table, Value},
    host::{HostCall, HostCallContext, HostReturn},
    logging::{LogReader, LogRecord},
    runner::{ErrorKind, RunOutcome, Vm, VmError},
    value::LuaValue,
};

/// A synchronous operation that may call function arguments without retaining them.
pub type ScopedServerCall<'a> =
    &'a dyn for<'call> Fn(HostCall<'call>) -> Result<HostReturn, VmError>;

/// A data-only host function borrowing the current World execution phase.
pub type ScopedServerFunction<'a> = &'a dyn Fn(&[LuaValue]) -> Result<Vec<LuaValue>, VmError>;

/// Detached callback target. It retains the VM, but becomes invalid when its Addon is replaced.
#[derive(Clone)]
pub struct AddonCallback {
    vm: Rc<RefCell<Vm>>,
    lua: Lua,
    environment: Table,
    slots: Table,
    names: Rc<Vec<String>>,
    stage: Rc<Cell<Stage>>,
    tick_disabled: Rc<Cell<bool>>,
    depth: Rc<Cell<usize>>,
    valid: Rc<Cell<bool>>,
    logs: LogReader,
    host_calls: HostCallContext,
}
pub(crate) struct ExecutionGuard(Rc<Cell<usize>>);
impl Drop for ExecutionGuard {
    fn drop(&mut self) {
        self.0.set(self.0.get() - 1);
    }
}
// Slots are preallocated at construction. Restore even when host execution unwinds
// or installing a later function fails; scoped functions must never remain selected.
struct SlotRestore<'a> {
    slots: &'a Table,
    prior: Vec<(usize, Value)>,
    stage: &'a Cell<Stage>,
}
impl Drop for SlotRestore<'_> {
    fn drop(&mut self) {
        for (name, value) in self.prior.drain(..) {
            if self.slots.raw_set(name, value).is_err() {
                self.stage.set(Stage::Failed);
            }
        }
    }
}
impl AddonCallback {
    pub(crate) fn new(
        vm: Rc<RefCell<Vm>>,
        stage: Rc<Cell<Stage>>,
        tick_disabled: Rc<Cell<bool>>,
        names: &[String],
    ) -> Result<Self, VmError> {
        let (lua, environment, slots) = vm.borrow_mut().configure(|lua, env| {
            let slots = lua.create_table()?;
            let server: Table = env.raw_get("server")?;
            // Script-held aliases resolve the current phase slot.
            for (index, name) in names.iter().enumerate() {
                slots.raw_set(index + 1, false)?;
                let wrapper: Function = lua.load("local slots, name, fail = ...; return function(...) local f = slots[name]; if not f then fail('server operation outside host phase') end; return f(...) end")
                    .set_name("=host server dispatch").call((slots.clone(), index + 1, lua.globals().get::<Function>("error")?))?;
                server.raw_set(name.as_str(), wrapper)?;
            }
            Ok((lua.clone(), env.clone(), slots))
        })?;
        let logs = vm.borrow().log_reader();
        let host_calls = vm.borrow().host_call_context();
        Ok(Self {
            vm,
            lua,
            environment,
            slots,
            names: Rc::new(names.to_vec()),
            stage,
            tick_disabled,
            depth: Rc::new(Cell::new(0)),
            valid: Rc::new(Cell::new(true)),
            logs,
            host_calls,
        })
    }
    pub(crate) fn enter(&self) -> ExecutionGuard {
        self.depth.set(self.depth.get() + 1);
        ExecutionGuard(Rc::clone(&self.depth))
    }
    pub(crate) fn invalidate(&self) {
        self.valid.set(false);
    }
    /// Names of borrowed server operations required by each host phase.
    pub fn scoped_server_functions(&self) -> &[String] {
        &self.names
    }
    /// Drain records even while a server operation is delivering nested callbacks.
    pub fn drain_log_records(&self) -> Vec<LogRecord> {
        self.logs.drain()
    }
    /// Deliver an event immediately. Nested events inherit the running coroutine's instruction budget.
    pub fn dispatch(&self, callback: &str, arguments: &[LuaValue]) -> Result<RunOutcome, VmError> {
        if !self.valid.get() || matches!(self.stage.get(), Stage::Failed | Stage::Destroyed) {
            return Err(VmError::new(
                ErrorKind::Failed,
                "addon callback target is no longer available",
            ));
        }
        if !storm_lua_spec::addon::EVENTS.contains(&callback) {
            return Err(invalid("unknown or reserved addon callback"));
        }
        let prior = self.stage.get();
        if matches!(prior, Stage::Empty) {
            return Err(invalid("addon source has not been loaded"));
        }
        let nested = self.depth.get() != 0;
        if !nested {
            self.vm.borrow().ensure_idle()?;
        }
        let _execution = self.enter();
        let result = if nested {
            (|| {
                let value: Value = self.environment.raw_get(callback)?;
                let Value::Function(function) = value else {
                    return Ok(RunOutcome::Missing);
                };
                let args = backend::encode_values(&self.lua, arguments)?;
                function.call::<MultiValue>(args).map_err(VmError::from)?;
                Ok(RunOutcome::Completed)
            })()
        } else {
            self.vm.borrow_mut().call_with(callback, arguments)
        };
        if let Err(error) = &result {
            if error.kind == ErrorKind::Lua {
                self.tick_disabled.set(true);
            } else {
                self.stage.set(Stage::Failed);
            }
        }
        result
    }
    /// Install borrowed server functions for a synchronous host phase, restoring outer bindings afterward.
    /// Cached Lua aliases remain valid across phases. The closure must finish before borrowed data expires.
    pub fn with_server_functions<R>(
        &self,
        functions: &[(&str, ScopedServerFunction<'_>)],
        run: impl FnOnce() -> Result<R, VmError>,
    ) -> Result<R, VmError> {
        self.with_server_calls(functions, &[], run)
    }
    /// Install ordinary data handlers and call-lifetime handlers in one phase.
    /// Their combined names must provide every configured operation exactly once.
    pub fn with_server_calls<R>(
        &self,
        functions: &[(&str, ScopedServerFunction<'_>)],
        calls: &[(&str, ScopedServerCall<'_>)],
        run: impl FnOnce() -> Result<R, VmError>,
    ) -> Result<R, VmError> {
        if !self.valid.get() {
            return Err(invalid("addon callback target is no longer available"));
        }
        if functions.len() + calls.len() != self.names.len()
            || self.names.iter().any(|name| {
                functions.iter().filter(|(key, _)| *key == name).count()
                    + calls.iter().filter(|(key, _)| *key == name).count()
                    != 1
            })
        {
            return Err(invalid(
                "host phase must provide each scoped server operation exactly once",
            ));
        }
        self.lua
            .scope(|scope| {
                let mut restore = SlotRestore {
                    slots: &self.slots,
                    prior: Vec::new(),
                    stage: &self.stage,
                };
                for (name, handler) in functions {
                    let function = scope.create_function(move |lua, args: MultiValue| {
                        let args = backend::decode_values(args)
                            .map_err(backend::BackendError::external)?;
                        let output = handler(&args).map_err(backend::BackendError::external)?;
                        backend::encode_values(lua, &output)
                            .map_err(backend::BackendError::external)
                    })?;
                    let index = self
                        .names
                        .iter()
                        .position(|key| key == name)
                        .ok_or_else(|| {
                            backend::BackendError::external(invalid("unknown scoped operation"))
                        })?
                        + 1;
                    restore
                        .prior
                        .push((index, self.slots.raw_get::<Value>(index)?));
                    self.slots.raw_set(index, function)?;
                }
                for (name, handler) in calls {
                    let context = &self.host_calls;
                    let function = scope.create_function(move |lua, args: MultiValue| {
                        context.invoke(lua, args, handler)
                    })?;
                    let index = self
                        .names
                        .iter()
                        .position(|key| key == name)
                        .ok_or_else(|| {
                            backend::BackendError::external(invalid("unknown scoped operation"))
                        })?
                        + 1;
                    restore
                        .prior
                        .push((index, self.slots.raw_get::<Value>(index)?));
                    self.slots.raw_set(index, function)?;
                }
                let result = run();
                let mut restore_error = None;
                while let Some((index, value)) = restore.prior.pop() {
                    if let Err(error) = self.slots.raw_set(index, value) {
                        self.stage.set(Stage::Failed);
                        if restore_error.is_none() {
                            restore_error = Some(error);
                        }
                    }
                }
                drop(restore);
                if let Some(error) = restore_error {
                    return Err(error);
                }
                Ok(result)
            })
            .map_err(VmError::from)?
    }
}
