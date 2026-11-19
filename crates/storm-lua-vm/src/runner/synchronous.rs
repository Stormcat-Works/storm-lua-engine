//! Synchronous host events share their owning VM and its live instruction budget.
use super::*;
use std::sync::{
    atomic::{AtomicBool, AtomicUsize, Ordering},
    Mutex, Weak,
};

const MAX_FAST_QUANTUM: u64 = 4096;

pub(crate) fn select_quantum(remaining: u64, line_debug: bool, synchronous_issued: bool) -> u32 {
    if line_debug || synchronous_issued {
        1
    } else {
        remaining.clamp(1, MAX_FAST_QUANTUM) as u32
    }
}

pub(super) struct Context {
    owner: Weak<()>,
    pass: Mutex<Weak<PassState>>,
    lua: Lua,
    environment: Table,
    control: Arc<Mutex<HookControl>>,
    limits: ExecutionLimits,
    pub(super) failed: Arc<AtomicBool>,
    pub(super) addon_callbacks: AtomicBool,
    pub(super) active: AtomicUsize,
    pub(super) issued: AtomicBool,
    pub(super) main_hook_state: AtomicUsize,
    frames: Mutex<Vec<(Thread, bool)>>,
    pub(super) restore_error: Mutex<Option<VmError>>,
    pub(super) main_pending: AtomicBool,
}
impl Context {
    pub(super) fn new(
        lua: &Lua,
        environment: &Table,
        control: Arc<Mutex<HookControl>>,
        limits: ExecutionLimits,
        owner: Weak<()>,
    ) -> Arc<Self> {
        Arc::new(Self {
            owner,
            pass: Mutex::new(Weak::new()),
            lua: lua.clone(),
            environment: environment.clone(),
            control,
            limits,
            failed: Arc::new(AtomicBool::new(false)),
            addon_callbacks: AtomicBool::new(false),
            active: AtomicUsize::new(0),
            issued: AtomicBool::new(false),
            main_hook_state: AtomicUsize::new(0),
            frames: Mutex::new(Vec::with_capacity(4)),
            restore_error: Mutex::new(None),
            main_pending: AtomicBool::new(false),
        })
    }
    pub(super) fn install_synchronous_hook(&self, thread: &Thread) {
        self.main_hook_state.store(0, Ordering::Relaxed);
        let quantum = 1_u32;
        lock(&self.control).active_quantum = quantum;
        let control = Arc::clone(&self.control);
        // Unlike the debuggable main continuation this protected host event cannot
        // yield. Count every instruction against the shared control so short or
        // nested synchronous callbacks cannot accumulate uncounted gas.
        thread.set_hook(
            HookTriggers::new().every_nth_instruction(quantum),
            move |_, info| {
                let mut state = lock(&control);
                if state.remaining <= u64::from(quantum) {
                    state.remaining = 0;
                    state.exhausted = true;
                } else {
                    state.remaining -= u64::from(quantum);
                }
                state.note_instructions(quantum);
                if state.time_exceeded {
                    state.limit_line = Some(info.curr_line());
                    Err(mlua::Error::RuntimeError(
                        "max execution time exceeded".into(),
                    ))
                } else if state.exhausted {
                    Err(mlua::Error::RuntimeError(
                        "instruction budget exceeded".into(),
                    ))
                } else {
                    Ok(VmState::Continue)
                }
            },
        );
    }
    pub(super) fn install_main_hook(&self, thread: &Thread) -> Result<(usize, bool), VmError> {
        let control = Arc::clone(&self.control);
        let probe = YieldProbe::new(&self.lua, thread)?;
        let state_id = probe.state_id();
        let synchronous_issued = self.issued.load(Ordering::Relaxed);
        let (quantum, line_debug) = {
            let mut state = lock(&self.control);
            #[cfg(feature = "debug")]
            let line_debug =
                !state.breakpoints.is_empty() || state.step.is_some() || state.pause_requested;
            #[cfg(not(feature = "debug"))]
            let line_debug = false;
            let q = select_quantum(state.remaining, line_debug, synchronous_issued);
            state.active_quantum = q;
            (q, line_debug)
        };
        let mut triggers = HookTriggers::new().every_nth_instruction(quantum);
        #[cfg(feature = "debug")]
        if line_debug {
            // デバッガサポートのコンパイルによって、通常の実行に行フックや呼び出しフックが追加されてはなりません。
            triggers = triggers.every_line().on_calls().on_returns();
        }
        #[cfg(not(feature = "debug"))]
        let _ = &mut triggers;
        thread.set_hook(triggers, move |_, info| {
            let mut state = lock(&control);
            let q = u64::from(state.active_quantum.max(1));
            let elapsed_q = q as u32;
            if info.event() == DebugEvent::Count {
                if state.remaining <= q {
                    state.remaining = 0;
                    state.exhausted = true;
                } else {
                    state.remaining -= q;
                    if state.remaining < q {
                        let next_q = state.remaining as u32;
                        state.active_quantum = next_q;
                        probe.tighten_count_hook(next_q);
                    }
                }
                state.note_instructions(elapsed_q);
            }
            if state.time_exceeded {
                // 時間制限も命令数バジェットと同じく、pcallで握りつぶせない形で入口を打ち切ります。
                state.limit_line = Some(info.curr_line());
                return if probe.can_yield() {
                    Ok(VmState::Yield)
                } else {
                    if state.active_quantum != 1 {
                        state.active_quantum = 1;
                        probe.tighten_count_hook(1);
                    }
                    Err(mlua::Error::RuntimeError(
                        "max execution time exceeded".into(),
                    ))
                };
            }
            if state.exhausted {
                // mluaはyield不可能なCコールバック内のYieldを暗黙に無視するため、そこではエラーを送出します。
                // yield可能な境界では代わりに中断することで、pcallがバジェット枯渇を握りつぶせないようにします。
                return if probe.can_yield() {
                    state.limit_line = Some(info.curr_line());
                    Ok(VmState::Yield)
                } else {
                    if state.active_quantum != 1 {
                        state.active_quantum = 1;
                        probe.tighten_count_hook(1);
                    }
                    Err(mlua::Error::RuntimeError(
                        "instruction budget exceeded".into(),
                    ))
                };
            }
            #[cfg(feature = "debug")]
            {
                if info.event() == DebugEvent::Count {
                    if state.pause_poll.is_some() {
                        state.poll_countdown = state.poll_countdown.saturating_sub(elapsed_q);
                    }
                    if state.poll_countdown == 0 {
                        if let Some(poll) = state.pause_poll.clone() {
                            state.poll_countdown = PAUSE_POLL_INTERVAL;
                            // ホストの関数を呼ぶ間は制御状態をロックしません。
                            drop(state);
                            let pause = poll().map_err(mlua::Error::external)?;
                            state = lock(&control);
                            state.poll_pending |= pause;
                        }
                    }
                    if state.poll_pending && probe.can_yield() {
                        state.poll_pending = false;
                        let source = info
                            .source()
                            .source
                            .map(|s| s.into_owned())
                            .unwrap_or_default();
                        state.last_line = Some((source, info.curr_line()));
                        state.local_names = probe.local_names()?;
                        return Ok(VmState::Yield);
                    }
                }
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
                    let stop = state.pause_requested
                        || state
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
                        state.pause_requested = false;
                        state.last_line = Some((source, line));
                        state.local_names = probe.local_names()?;
                        return Ok(VmState::Yield);
                    }
                }
            }
            Ok(VmState::Continue)
        });
        self.main_hook_state.store(state_id, Ordering::Relaxed);
        Ok((state_id, line_debug))
    }
}
// mlua stores one hook callback/thread per Lua state, shared by all coroutines.
// Always restore the caller, including errors, nested calls and unwinding.
//
// Lock discipline: the frame stack lock is released before any hook install, and
// no `Context` lock is ever held across a Lua call or across this drop path's
// re-entry into `Context` (`install_main_hook` / `install_synchronous_hook`),
// both of which lock `HookControl` themselves.
pub(super) struct ActiveCall<'a>(&'a Context, bool);
impl<'a> ActiveCall<'a> {
    pub(super) fn enter(context: &'a Context, thread: &Thread, main: bool) -> Self {
        let track_frame = !main || context.issued.load(Ordering::Relaxed);
        if track_frame {
            lock(&context.frames).push((thread.clone(), main));
        }
        context.active.fetch_add(1, Ordering::Relaxed);
        Self(context, track_frame)
    }
}
impl Drop for ActiveCall<'_> {
    fn drop(&mut self) {
        if !self.1 {
            self.0.active.fetch_sub(1, Ordering::Relaxed);
            return;
        }
        // Scoped: pop and clone the previous frame, then release before the hook
        // reinstall below (which takes the HookControl lock).
        let previous = {
            let mut frames = lock(&self.0.frames);
            frames.pop();
            frames.last().cloned()
        };
        self.0.active.fetch_sub(1, Ordering::Relaxed);
        if let Some((thread, main)) = previous {
            let result = if main {
                self.0.install_main_hook(&thread).map(|_| ())
            } else {
                self.0.install_synchronous_hook(&thread);
                Ok(())
            };
            if let Err(error) = result {
                self.0.failed.store(true, Ordering::Relaxed);
                *lock(&self.0.restore_error) = Some(error);
            }
        }
    }
}

struct PassState {
    pending_failure: AtomicBool,
}
/// An explicit host-call scope. Ordinary callback errors become terminal when
/// this guard drops; resource limits remain terminal immediately.
pub struct SynchronousCallbackPass {
    context: Weak<Context>,
    state: Arc<PassState>,
}
impl SynchronousCallbackPass {
    /// Get an entry valid only until this pass ends.
    pub fn callbacks(&self) -> SynchronousCallbacks {
        SynchronousCallbacks {
            context: self.context.clone(),
            pass: Some(Arc::downgrade(&self.state)),
        }
    }
}
impl Drop for SynchronousCallbackPass {
    fn drop(&mut self) {
        if self.state.pending_failure.load(Ordering::Relaxed) {
            if let Some(context) = self.context.upgrade() {
                context.failed.store(true, Ordering::Relaxed);
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
    fn context(&self) -> Result<Arc<Context>, VmError> {
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
        if context.failed.load(Ordering::Relaxed) {
            return Err(VmError::new(ErrorKind::Failed, "callback owner has failed"));
        }
        if context.active.load(Ordering::Relaxed) == 0
            && context.main_pending.load(Ordering::Relaxed)
        {
            return Err(VmError::new(ErrorKind::Busy, "main callback is suspended"));
        }
        Ok(context)
    }
    /// Begin one explicit host pass; overlapping passes are rejected.
    pub fn begin_pass(&self) -> Result<SynchronousCallbackPass, VmError> {
        let context = self.context()?;
        if lock(&context.pass).upgrade().is_some() {
            return Err(VmError::new(
                ErrorKind::Busy,
                "synchronous pass already active",
            ));
        }
        if context.active.load(Ordering::Relaxed) == 0 {
            lock(&context.control).begin_entry(&context.limits);
        }
        let state = Arc::new(PassState {
            pending_failure: AtomicBool::new(false),
        });
        *lock(&context.pass) = Arc::downgrade(&state);
        Ok(SynchronousCallbackPass {
            context: Arc::downgrade(&context),
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
    /// Callback errors retain Lua side effects. Ordinary Lua errors in the Addon
    /// profile remain recoverable; other failures follow the optional pass scope.
    /// A synchronous callback cannot suspend before its host operation returns.
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
                context.failed.store(true, Ordering::Relaxed);
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
        let active_depth = context.active.load(Ordering::Relaxed);
        if active_depth == 0 && self.pass.is_none() {
            lock(&context.control).begin_entry(&context.limits);
        } else if lock(&context.control).exhausted {
            context.failed.store(true, Ordering::Relaxed);
            return Err(VmError::new(
                ErrorKind::Limit,
                "instruction budget exceeded",
            ));
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
            thread.resume::<()>(arguments)
        };
        if let Some(error) = lock(&context.restore_error).take() {
            context.failed.store(true, Ordering::Relaxed);
            return Err(error);
        }
        let attempted_yield = result.is_ok() && thread.status() == ThreadStatus::Resumable;
        let outcome = if lock(&context.control).time_exceeded {
            let line = {
                let mut control = lock(&context.control);
                control.time_exceeded = false;
                if context.active.load(Ordering::Relaxed) == 0 && self.pass.is_none() {
                    control.deadline = None;
                } else {
                    control.time_countdown = 1;
                }
                control.limit_line.take()
            };
            let mut error = VmError::new(ErrorKind::Lua, context.limits.timeout_message(line));
            attach_trace(&context.lua, &thread, &mut error, None, line);
            Err(error)
        } else if lock(&context.control).exhausted {
            let mut error = VmError::new(ErrorKind::Limit, "instruction budget exceeded");
            attach_trace(&context.lua, &thread, &mut error, None, None);
            Err(error)
        } else {
            match result {
                Err(error) => {
                    let mut error = VmError::from(error);
                    attach_trace(&context.lua, &thread, &mut error, None, None);
                    Err(error)
                }
                Ok(_) if thread.status() == ThreadStatus::Resumable => Err(VmError::new(
                    ErrorKind::Lua,
                    "synchronous callback attempted to yield",
                )),
                Ok(_) => Ok(RunOutcome::Completed),
            }
        };
        if let Err(error) = &outcome {
            if context.addon_callbacks.load(Ordering::Relaxed)
                && error.kind == ErrorKind::Lua
                && !attempted_yield
            {
                return outcome;
            }
            if let Some(pass) = self
                .pass
                .as_ref()
                .and_then(Weak::upgrade)
                .filter(|_| error.kind != ErrorKind::Limit && !attempted_yield)
            {
                pass.pending_failure.store(true, Ordering::Relaxed);
            } else {
                context.failed.store(true, Ordering::Relaxed);
            }
        }
        outcome
    }
}
impl Vm {
    /// Obtain an owned entry for synchronous callbacks from explicit host services.
    pub fn synchronous_callbacks(&self) -> SynchronousCallbacks {
        self.synchronous.issued.store(true, Ordering::Relaxed);
        SynchronousCallbacks {
            context: Arc::downgrade(&self.synchronous),
            pass: None,
        }
    }
}
