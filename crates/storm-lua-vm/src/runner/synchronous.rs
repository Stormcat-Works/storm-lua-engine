//! Synchronous host events share their owning VM and its live instruction budget.
use super::*;
use std::{cell::Cell, rc::Weak};

pub(super) struct Context {
    owner: Weak<()>,
    pass: RefCell<Weak<PassState>>,
    lua: Lua,
    environment: Table,
    control: Rc<RefCell<HookControl>>,
    limits: ExecutionLimits,
    pub(super) failed: Rc<Cell<bool>>,
    pub(super) active: Cell<usize>,
    frames: RefCell<Vec<(Thread, bool)>>,
    pub(super) restore_error: RefCell<Option<VmError>>,
    pub(super) main_pending: Cell<bool>,
}
impl Context {
    pub(super) fn new(
        lua: &Lua,
        environment: &Table,
        control: Rc<RefCell<HookControl>>,
        limits: ExecutionLimits,
        owner: Weak<()>,
    ) -> Rc<Self> {
        Rc::new(Self {
            owner,
            pass: RefCell::new(Weak::new()),
            lua: lua.clone(),
            environment: environment.clone(),
            control,
            limits,
            failed: Rc::new(Cell::new(false)),
            active: Cell::new(0),
            frames: RefCell::new(Vec::new()),
            restore_error: RefCell::new(None),
            main_pending: Cell::new(false),
        })
    }
    fn install_synchronous_hook(&self, thread: &Thread) {
        let quantum = 1_u32;
        let control = self.control.clone();
        // Unlike the debuggable main continuation this protected host event cannot
        // yield. Count against the same control so nested events cannot reset gas.
        thread.set_hook(
            HookTriggers::new().every_nth_instruction(quantum),
            move |_, _| {
                let mut state = control.borrow_mut();
                if state.remaining <= u64::from(quantum) {
                    state.remaining = 0;
                    state.exhausted = true;
                } else {
                    state.remaining -= u64::from(quantum);
                }
                if state.exhausted {
                    Err(mlua::Error::RuntimeError(
                        "instruction budget exceeded".into(),
                    ))
                } else {
                    Ok(VmState::Continue)
                }
            },
        );
    }
    pub(super) fn install_main_hook(&self, thread: &Thread) -> Result<(), VmError> {
        let control = Rc::clone(&self.control);
        let probe = YieldProbe::new(&self.lua, thread)?;
        let quantum = 1_u32;
        let mut triggers = HookTriggers::new().every_nth_instruction(quantum);
        #[cfg(feature = "debug")]
        {
            // デバッガサポートのコンパイルによって、通常の実行に行フックや呼び出しフックが追加されてはなりません。
            let state = self.control.borrow();
            if !state.breakpoints.is_empty() || state.step.is_some() {
                triggers = triggers.every_line().on_calls().on_returns();
            }
        }
        #[cfg(not(feature = "debug"))]
        let _ = &mut triggers;
        thread.set_hook(triggers, move |_, info| {
            let mut state = control.borrow_mut();
            if info.event() == DebugEvent::Count {
                if state.remaining <= u64::from(quantum) {
                    state.remaining = 0;
                    state.exhausted = true;
                } else {
                    state.remaining -= u64::from(quantum);
                }
            }
            if state.exhausted {
                // mluaはyield不可能なCコールバック内のYieldを暗黙に無視するため、そこではエラーを送出します。
                // yield可能な境界では代わりに中断することで、pcallがバジェット枯渇を握りつぶせないようにします。
                return if probe.can_yield() {
                    Ok(VmState::Yield)
                } else {
                    Err(mlua::Error::RuntimeError(
                        "instruction budget exceeded".into(),
                    ))
                };
            }
            #[cfg(feature = "debug")]
            {
                match info.event() {
                    DebugEvent::Call => state.depth += 1,
                    DebugEvent::Ret => state.depth -= 1,
                    _ => {}
                }
                if info.event() == DebugEvent::Line {
                    let source = info
                        .source()
                        .source
                        .map(|s| s.into_owned())
                        .unwrap_or_default();
                    let line = info.curr_line();
                    if state.skip_first_line {
                        state.skip_first_line = false;
                        if state.last_line.as_ref() == Some(&(source.clone(), line)) {
                            return Ok(VmState::Continue);
                        }
                    }
                    let stop = state
                        .breakpoints
                        .iter()
                        .any(|(s, l)| *l == line && (s.is_empty() || *s == source))
                        || match state.step {
                            Some(StepMode::Into) => true,
                            Some(StepMode::Over) => state.depth <= state.target_depth,
                            Some(StepMode::Out) => state.depth < state.target_depth,
                            _ => false,
                        };
                    if stop && probe.can_yield() {
                        state.last_line = Some((source, line));
                        state.local_names = probe.local_names()?;
                        return Ok(VmState::Yield);
                    }
                }
            }
            Ok(VmState::Continue)
        });
        Ok(())
    }
}
// mlua stores one hook callback/thread per Lua state, shared by all coroutines.
// Always restore the caller, including errors, nested calls and unwinding.
pub(super) struct ActiveCall<'a>(&'a Context);
impl<'a> ActiveCall<'a> {
    pub(super) fn enter(context: &'a Context, thread: &Thread, main: bool) -> Self {
        context.frames.borrow_mut().push((thread.clone(), main));
        context.active.set(context.active.get() + 1);
        Self(context)
    }
}
impl Drop for ActiveCall<'_> {
    fn drop(&mut self) {
        let previous = {
            let mut frames = self.0.frames.borrow_mut();
            frames.pop();
            frames.last().cloned()
        };
        self.0.active.set(self.0.active.get() - 1);
        if let Some((thread, main)) = previous {
            let result = if main {
                self.0.install_main_hook(&thread)
            } else {
                self.0.install_synchronous_hook(&thread);
                Ok(())
            };
            if let Err(error) = result {
                self.0.failed.set(true);
                *self.0.restore_error.borrow_mut() = Some(error);
            }
        }
    }
}

struct PassState {
    pending_failure: Cell<bool>,
}
/// An explicit host-call scope. Ordinary callback errors become terminal when
/// this guard drops; resource limits remain terminal immediately.
pub struct SynchronousCallbackPass {
    context: Weak<Context>,
    state: Rc<PassState>,
}
impl SynchronousCallbackPass {
    /// Get an entry valid only until this pass ends.
    pub fn callbacks(&self) -> SynchronousCallbacks {
        SynchronousCallbacks {
            context: self.context.clone(),
            pass: Some(Rc::downgrade(&self.state)),
        }
    }
}
impl Drop for SynchronousCallbackPass {
    fn drop(&mut self) {
        if self.state.pending_failure.get() {
            if let Some(context) = self.context.upgrade() {
                context.failed.set(true);
            }
        }
    }
}

/// A thread-bound event entry for synchronous callbacks from host functions.
///
/// Nested calls share the active VM's instruction budget. Idle calls start a new
/// budget; suspended main callbacks reject overlap. Dropping the VM invalidates
/// the handle. No raw backend access or mutable VM borrow is exposed.
#[derive(Clone)]
pub struct SynchronousCallbacks {
    context: Weak<Context>,
    pass: Option<Weak<PassState>>,
}
impl SynchronousCallbacks {
    fn context(&self) -> Result<Rc<Context>, VmError> {
        if self
            .pass
            .as_ref()
            .is_some_and(|pass| pass.upgrade().is_none())
        {
            return Err(VmError::new(
                ErrorKind::Failed,
                "synchronous pass has ended",
            ));
        }
        let context = self
            .context
            .upgrade()
            .ok_or_else(|| VmError::new(ErrorKind::Failed, "callback owner was dropped"))?;
        if context.owner.upgrade().is_none() {
            return Err(VmError::new(
                ErrorKind::Failed,
                "callback owner was dropped",
            ));
        }
        if context.failed.get() {
            return Err(VmError::new(ErrorKind::Failed, "callback owner has failed"));
        }
        if context.active.get() == 0 && context.main_pending.get() {
            return Err(VmError::new(ErrorKind::Busy, "main callback is suspended"));
        }
        Ok(context)
    }
    /// Begin one explicit host pass; overlapping passes are rejected.
    pub fn begin_pass(&self) -> Result<SynchronousCallbackPass, VmError> {
        let context = self.context()?;
        if context.pass.borrow().upgrade().is_some() {
            return Err(VmError::new(
                ErrorKind::Busy,
                "synchronous pass already active",
            ));
        }
        if context.active.get() == 0 {
            let mut control = context.control.borrow_mut();
            control.remaining = context.limits.instruction_budget.get();
            control.exhausted = false;
        }
        let state = Rc::new(PassState {
            pending_failure: Cell::new(false),
        });
        *context.pass.borrow_mut() = Rc::downgrade(&state);
        Ok(SynchronousCallbackPass {
            context: Rc::downgrade(&context),
            state,
        })
    }
    /// Whether the owner and optional pass remain live, including a paused main call.
    pub fn is_live(&self) -> bool {
        matches!(
            self.context(),
            Ok(_)
                | Err(VmError {
                    kind: ErrorKind::Busy,
                    ..
                })
        )
    }
    /// Check owner lifetime, failure and suspended-main exclusion without executing Lua.
    pub fn ensure_available(&self) -> Result<(), VmError> {
        self.context()?;
        Ok(())
    }
    /// Invoke a named callback immediately, including while its VM calls a host function.
    /// Callback errors retain Lua side effects and mark the VM failed. A synchronous
    /// callback cannot suspend; it must complete before its host operation returns.
    pub fn call(
        &self,
        name: &str,
        arguments: &[crate::value::LuaValue],
    ) -> Result<RunOutcome, VmError> {
        self.call_inner(name, arguments, false)
    }
    /// Addon event lookup: a missing or nonfunction global is an absent handler.
    pub fn call_if_function(
        &self,
        name: &str,
        arguments: &[crate::value::LuaValue],
    ) -> Result<RunOutcome, VmError> {
        self.call_inner(name, arguments, true)
    }
    fn call_inner(
        &self,
        name: &str,
        arguments: &[crate::value::LuaValue],
        ignore_nonfunction: bool,
    ) -> Result<RunOutcome, VmError> {
        let context = self.context()?;
        let boundary_error = |error: VmError| {
            if error.kind == ErrorKind::Limit {
                context.failed.set(true);
            }
            error
        };
        let function = match context
            .environment
            .raw_get::<Value>(name)
            .map_err(VmError::from)
            .map_err(boundary_error)?
        {
            Value::Nil => return Ok(RunOutcome::Missing),
            Value::Function(f) => f,
            _ if ignore_nonfunction => return Ok(RunOutcome::Missing),
            _ => {
                return Err(VmError::new(
                    ErrorKind::InvalidArgument,
                    format!("{name} is not a function"),
                ))
            }
        };
        if context.active.get() == 0 && self.pass.is_none() {
            let mut control = context.control.borrow_mut();
            control.remaining = context.limits.instruction_budget.get();
            control.exhausted = false;
        }
        let arguments = crate::value::encode(&context.lua, arguments).map_err(boundary_error)?;
        let thread = context
            .lua
            .create_thread(function)
            .map_err(VmError::from)
            .map_err(boundary_error)?;
        context.install_synchronous_hook(&thread);
        let result = {
            let _active = ActiveCall::enter(&context, &thread, false);
            thread.resume::<MultiValue>(arguments)
        };
        if let Some(error) = context.restore_error.borrow_mut().take() {
            context.failed.set(true);
            return Err(error);
        }
        let attempted_yield = result.is_ok() && thread.status() == ThreadStatus::Resumable;
        let outcome = if context.control.borrow().exhausted {
            Err(VmError::new(
                ErrorKind::Limit,
                "instruction budget exceeded",
            ))
        } else {
            match result {
                Err(error) => Err(error.into()),
                Ok(_) if thread.status() == ThreadStatus::Resumable => Err(VmError::new(
                    ErrorKind::Lua,
                    "synchronous callback attempted to yield",
                )),
                Ok(_) => Ok(RunOutcome::Completed),
            }
        };
        if let Err(error) = &outcome {
            if let Some(pass) = self
                .pass
                .as_ref()
                .and_then(Weak::upgrade)
                .filter(|_| error.kind != ErrorKind::Limit && !attempted_yield)
            {
                pass.pending_failure.set(true);
            } else {
                context.failed.set(true);
            }
        }
        outcome
    }
}
impl Vm {
    /// Obtain an owned entry for synchronous callbacks from explicit host services.
    pub fn synchronous_callbacks(&self) -> SynchronousCallbacks {
        SynchronousCallbacks {
            context: Rc::downgrade(&self.synchronous),
            pass: None,
        }
    }
}
