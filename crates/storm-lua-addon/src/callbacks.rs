//! Borrowed host phases and synchronous callback delivery share one Lua state.
use super::{invalid, stage_of, ErrorState, Stage};
use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc, Mutex,
    },
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
    synchronous: storm_lua_vm::runner::SynchronousCallbacks,
    lua: Lua,
    slots: Table,
    wrappers: Arc<Vec<Function>>,
    names: Arc<Vec<String>>,
    /// 構築時に確定する名前 -> スロット番号（1始まり）の索引。名前集合は
    /// 構築後に変わらないため、フェーズごとの検証と割当てはこの索引で
    /// 線形時間に行える。
    name_slots: Arc<HashMap<String, usize>>,
    stage: Arc<Mutex<Stage>>,
    error_state: Arc<ErrorState>,
    depth: Arc<AtomicUsize>,
    valid: Arc<AtomicBool>,
    logs: LogReader,
    host_calls: HostCallContext,
}
pub(crate) struct ExecutionGuard(Arc<AtomicUsize>);
impl Drop for ExecutionGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Relaxed);
    }
}
// Slots are preallocated at construction. Restore even when host execution unwinds
// or installing a later function fails; scoped functions must never remain selected.
struct SlotRestore<'a> {
    slots: &'a Table,
    prior: Vec<(usize, Value)>,
    stage: &'a Mutex<Stage>,
}
impl Drop for SlotRestore<'_> {
    fn drop(&mut self) {
        for (name, value) in self.prior.drain(..) {
            if self.slots.raw_set(name, value).is_err() {
                *storm_lua_vm::runner::lock(self.stage) = Stage::Failed;
            }
        }
    }
}
impl AddonCallback {
    pub(crate) fn new(
        vm: &mut Vm,
        stage: Arc<Mutex<Stage>>,
        error_state: Arc<ErrorState>,
        names: &[String],
    ) -> Result<Self, VmError> {
        let (lua, slots, wrappers) = vm.configure(|lua, env| {
            let slots = lua.create_table()?;
            let server: Table = env.raw_get("server")?;
            let mut wrappers = Vec::with_capacity(names.len());
            // Script-held aliases resolve the current phase slot.
            for (index, name) in names.iter().enumerate() {
                slots.raw_set(index + 1, false)?;
                let wrapper: Function = lua.load("local slots, name, fail = ...; return function(...) local f = slots[name]; if not f then fail('server operation outside host phase') end; return f(...) end")
                    .set_name("=host server dispatch").call((slots.clone(), index + 1, lua.globals().get::<Function>("error")?))?;
                server.raw_set(name.as_str(), wrapper.clone())?;
                wrappers.push(wrapper);
            }
            Ok((lua.clone(), slots, wrappers))
        })?;
        let (logs, host_calls, synchronous) = (
            vm.log_reader(),
            vm.host_call_context(),
            vm.synchronous_callbacks(),
        );
        let name_slots: HashMap<String, usize> = names
            .iter()
            .enumerate()
            .map(|(index, name)| (name.clone(), index + 1))
            .collect();
        Ok(Self {
            synchronous,
            lua,
            slots,
            wrappers: Arc::new(wrappers),
            names: Arc::new(names.to_vec()),
            name_slots: Arc::new(name_slots),
            stage,
            error_state,
            depth: Arc::new(AtomicUsize::new(0)),
            valid: Arc::new(AtomicBool::new(true)),
            logs,
            host_calls,
        })
    }
    pub(crate) fn enter(&self) -> ExecutionGuard {
        self.depth.fetch_add(1, Ordering::Relaxed);
        ExecutionGuard(Arc::clone(&self.depth))
    }
    pub(crate) fn invalidate(&self) {
        self.valid.store(false, Ordering::Relaxed);
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
        if !self.valid.load(Ordering::Relaxed)
            || matches!(stage_of(&self.stage), Stage::Failed | Stage::Destroyed)
        {
            return Err(VmError::new(
                ErrorKind::Failed,
                "addon callback target is no longer available",
            ));
        }
        if !storm_lua_spec::addon::EVENTS.contains(&callback) {
            return Err(invalid("unknown or reserved addon callback"));
        }
        let prior = stage_of(&self.stage);
        if matches!(prior, Stage::Empty) {
            return Err(invalid("addon source has not been loaded"));
        }
        self.synchronous.ensure_available()?;
        let _execution = self.enter();
        let result = self.synchronous.call_if_function(callback, arguments);
        if let Err(error) = &result {
            if error.kind == ErrorKind::Lua {
                // 通常イベントの失敗はステータス5として記録します（E9）。
                self.error_state.record(5, &error.message);
            } else {
                *storm_lua_vm::runner::lock(&self.stage) = Stage::Failed;
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
        if !self.valid.load(Ordering::Relaxed) {
            return Err(invalid("addon callback target is no longer available"));
        }
        // 検証は構築時索引で線形時間に行う。意味は従来どおり: 合計数が
        // 一致し、構成済みの各操作がちょうど1回ずつ提供されること
        // （未知の名前・重複・欠落はいずれも同じエラーで拒否する）。
        let mut provided = vec![false; self.names.len()];
        let mut exact = functions.len() + calls.len() == self.names.len();
        if exact {
            for name in functions
                .iter()
                .map(|(name, _)| name)
                .chain(calls.iter().map(|(name, _)| name))
            {
                match self.name_slots.get(*name) {
                    Some(&slot) if !provided[slot - 1] => provided[slot - 1] = true,
                    _ => {
                        exact = false;
                        break;
                    }
                }
            }
            exact = exact && provided.iter().all(|done| *done);
        }
        if !exact {
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
                    // 検証済みのため索引に必ず存在する。防御的に従来と
                    // 同じエラーを返す経路は残す。
                    let index = self.name_slots.get(*name).copied().ok_or_else(|| {
                        backend::BackendError::external(invalid("unknown scoped operation"))
                    })?;
                    restore
                        .prior
                        .push((index, self.slots.raw_get::<Value>(index)?));
                    self.slots.raw_set(index, function)?;
                }
                for (name, handler) in calls {
                    let index = self.name_slots.get(*name).copied().ok_or_else(|| {
                        backend::BackendError::external(invalid("unknown scoped operation"))
                    })?;
                    let context = &self.host_calls;
                    let wrapper = &self.wrappers[index - 1];
                    let function = scope.create_function(move |lua, args: MultiValue| {
                        context.invoke_with_function(lua, args, wrapper, handler)
                    })?;
                    restore
                        .prior
                        .push((index, self.slots.raw_get::<Value>(index)?));
                    self.slots.raw_set(index, function)?;
                }
                let result = run();
                let mut restore_error = None;
                while let Some((index, value)) = restore.prior.pop() {
                    if let Err(error) = self.slots.raw_set(index, value) {
                        *storm_lua_vm::runner::lock(&self.stage) = Stage::Failed;
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
