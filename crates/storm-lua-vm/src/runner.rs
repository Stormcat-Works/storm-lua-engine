//! バジェット管理されたLua実行および継続。ファイルシステム、ネットワーク、スケジューラは管理しません。
use crate::{
    backend_support::{create_lua, YieldProbe},
    ExecutionLimits,
};
use mlua::{
    ChunkMode, DebugEvent, Function, HookTriggers, Lua, MultiValue, Table, Thread, ThreadStatus,
    Value, VmState,
};
use std::{cell::RefCell, fmt, rc::Rc};

/// ホストAPIおよび外部関数アダプタで使用される安定したエラーカテゴリ。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorKind {
    /// コールバックが中断中、または既に実行中。
    Busy,
    /// 致命的な実行時エラーの発生後、実行を継続できません。
    Failed,
    /// 命令数、メモリ、ソースサイズ、またはホストバッファのバジェットを超過。
    Limit,
    /// スクリプトのコンパイルまたは実行時エラー。
    Lua,
    /// 不正なホスト引数、テキスト、または設定。
    InvalidArgument,
    /// このビルドまたはプロバイダでは機能がサポートされていません。
    Unsupported,
    /// 提供されたホストサービスが失敗。
    Host,
}
/// 診断テキストを伴う明示的なエンジンエラー。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VmError {
    /// 機械可読なカテゴリ。
    pub kind: ErrorKind,
    /// 診断の詳細情報（空の成功に置換されることはありません）。
    pub message: String,
}
impl VmError {
    /// ホストドメインのエラーを構築します。
    pub fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }
}
impl fmt::Display for VmError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}: {}", self.kind, self.message)
    }
}
impl std::error::Error for VmError {}
impl From<mlua::Error> for VmError {
    fn from(value: mlua::Error) -> Self {
        fn category(error: &mlua::Error) -> ErrorKind {
            match error {
                mlua::Error::MemoryError(_) => ErrorKind::Limit,
                mlua::Error::CallbackError { cause, .. } => category(cause),
                mlua::Error::ExternalError(cause) => cause
                    .downcast_ref::<VmError>()
                    .map_or(ErrorKind::Lua, |error| error.kind),
                _ => ErrorKind::Lua,
            }
        }
        Self::new(category(&value), value.to_string())
    }
}
/// 実行結果。エラーの表現には誤解を招く完了フラグではなくResultを使用します。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunOutcome {
    /// 指定された名前のコールバックが存在しませんでした。
    Missing,
    /// 呼び出しが正常に完了しました。
    Completed,
    /// ホストデバッガのためにその継続が中断（一時停止）しています。
    Suspended,
}
/// 中断中のコールバックを再開する際に適用されるステップ実行モード。
#[cfg(feature = "debug")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepMode {
    /// ブレークポイントまたは完了まで実行します。
    Continue,
    /// 次のソース行で停止します（ステップイン）。
    Into,
    /// 同一またはより浅い呼び出し深度の次の行で停止します（ステップオーバー）。
    Over,
    /// 現在の呼び出し深度から脱出した後に停止します（ステップアウト）。
    Out,
}
#[derive(Default)]
pub(crate) struct HookControl {
    pub(crate) remaining: u64,
    pub(crate) exhausted: bool,
    #[cfg(feature = "debug")]
    pub(crate) breakpoints: Vec<(String, i32)>,
    #[cfg(feature = "debug")]
    pub(crate) step: Option<StepMode>,
    #[cfg(feature = "debug")]
    pub(crate) depth: i32,
    #[cfg(feature = "debug")]
    pub(crate) target_depth: i32,
    #[cfg(feature = "debug")]
    pub(crate) last_line: Option<(String, i32)>,
    #[cfg(feature = "debug")]
    pub(crate) local_names: Vec<(i32, Vec<u8>)>,
    #[cfg(feature = "debug")]
    pub(crate) skip_first_line: bool,
}

/// 1つのスレッド拘束型Lua状態。ホストコードがすべての呼び出しと継続を明示的に駆動します。
pub struct Vm {
    pub(crate) lua: Lua,
    pub(crate) environment: Table,
    pub(crate) pending: Option<Thread>,
    pub(crate) control: Rc<RefCell<HookControl>>,
    pub(crate) limits: ExecutionLimits,
    failed: bool,
    pub(crate) environment_profile: storm_lua_spec::environment::EnvironmentProfile,
    pub(crate) logs: Rc<RefCell<crate::logging::LogBuffer>>,
    #[cfg(feature = "debug")]
    pub(crate) debugger: crate::debug::Debugger,
}
impl Vm {
    /// ユーザーコードを実行せずに、許可リスト付きの環境を作成します。
    pub fn new(limits: ExecutionLimits) -> Result<Self, VmError> {
        Self::with_environment(
            limits,
            storm_lua_spec::environment::EnvironmentProfile::Game,
        )
    }
    /// Create an explicit script environment; host debugging is independent from this profile.
    pub fn with_environment(
        limits: ExecutionLimits,
        environment_profile: storm_lua_spec::environment::EnvironmentProfile,
    ) -> Result<Self, VmError> {
        let lua = create_lua()?;
        if lua.used_memory() > limits.lua_memory_bytes.get() {
            return Err(VmError::new(
                ErrorKind::Limit,
                "Lua memory budget is smaller than initialization",
            ));
        }
        lua.set_memory_limit(limits.lua_memory_bytes.get())?;
        let environment = crate::sandbox::environment(&lua, environment_profile)?;
        let control = Rc::new(RefCell::new(HookControl::default()));
        if environment_profile == storm_lua_spec::environment::EnvironmentProfile::Extended {
            crate::sandbox::protect_calls(&lua, &environment, Rc::clone(&control))?;
        }
        let logs = Rc::default();
        crate::logging::install(
            &lua,
            &environment,
            Rc::clone(&logs),
            environment_profile == storm_lua_spec::environment::EnvironmentProfile::Extended,
        )?;
        #[cfg(feature = "debug")]
        let debugger = crate::debug::Debugger::new(&lua)?;
        Ok(Self {
            lua,
            environment,
            pending: None,
            control,
            limits,
            failed: false,
            environment_profile,
            logs,
            #[cfg(feature = "debug")]
            debugger,
        })
    }
    /// Install host-shared Windows CRT math.random/randomseed semantics while idle.
    /// Lua argument/distribution/seed-discard behavior remains owned by this VM.
    pub fn install_crt_random(
        &mut self,
        source: crate::random::SharedCrtRandom,
    ) -> Result<(), VmError> {
        self.ensure_idle()?;
        crate::random::install(&self.lua, &self.environment.get::<Table>("math")?, source)?;
        Ok(())
    }
    /// バックエンド密結合な明示的拡張フィーチャーを通じてホストバインディングを設定します。
    #[cfg(feature = "backend-mlua")]
    pub fn configure<T>(
        &mut self,
        install: impl FnOnce(&Lua, &Table) -> mlua::Result<T>,
    ) -> Result<T, VmError> {
        self.ensure_idle()?;
        Ok(install(&self.lua, &self.environment)?)
    }
    /// 中断中のコールバックと重複する、または故障状態を再利用しようとするコマンドを拒否します。
    pub fn ensure_idle(&self) -> Result<(), VmError> {
        if self.failed {
            return Err(VmError::new(
                ErrorKind::Failed,
                "reset the VM after failure",
            ));
        }
        if self.pending.is_some() {
            return Err(VmError::new(ErrorKind::Busy, "callback is suspended"));
        }
        Ok(())
    }
    /// コールバックの継続が保持されている（中断中である）かどうか。
    pub fn is_suspended(&self) -> bool {
        self.pending.is_some()
    }
    /// 実行が失敗したかどうか。リセットするにはホストがこのインスタンスを再作成する必要があります。
    pub fn is_failed(&self) -> bool {
        self.failed
    }
    /// ホストバッファを除いた現在のLuaヒープ使用量。
    pub fn used_memory(&self) -> usize {
        self.lua.used_memory()
    }
    /// テキストのみをコンパイルし、ホストバインディング組み込み後にそのトップレベルを実行します。
    pub fn execute(&mut self, source: &[u8], name: &str) -> Result<RunOutcome, VmError> {
        self.ensure_idle()?;
        crate::source::validate_source(source, name)?;
        let function = self
            .lua
            .load(source)
            .set_name(name)
            .set_mode(ChunkMode::Text)
            .set_environment(self.environment.clone())
            .into_function()?;
        self.start(function, MultiValue::new())
    }
    /// 生のルックアップにより引数なしコールバックを呼び出します。
    pub fn call(&mut self, name: &str) -> Result<RunOutcome, VmError> {
        self.call_with(name, &[])
    }
    /// 末尾のnilを保持しつつ、損失のない所有引数を用いてコールバックを呼び出します。
    pub fn call_with(
        &mut self,
        name: &str,
        arguments: &[crate::value::LuaValue],
    ) -> Result<RunOutcome, VmError> {
        self.ensure_idle()?;
        match self.environment.raw_get::<Value>(name)? {
            Value::Nil => Ok(RunOutcome::Missing),
            Value::Function(function) => {
                self.start(function, crate::value::encode(&self.lua, arguments)?)
            }
            _ => Err(VmError::new(
                ErrorKind::InvalidArgument,
                format!("{name} is not a function"),
            )),
        }
    }
    fn start(&mut self, function: Function, arguments: MultiValue) -> Result<RunOutcome, VmError> {
        self.ensure_idle()?;
        {
            let mut control = self.control.borrow_mut();
            control.remaining = self.limits.instruction_budget.get();
            control.exhausted = false;
            #[cfg(feature = "debug")]
            {
                control.depth = 0;
                control.step = None;
                control.skip_first_line = false;
            }
        }
        let thread = self.lua.create_thread(function)?;
        self.install_hook(&thread)?;
        self.pending = Some(thread);
        self.resume_inner(arguments)
    }
    pub(crate) fn install_hook(&self, thread: &Thread) -> Result<(), VmError> {
        let control = Rc::clone(&self.control);
        let probe = YieldProbe::new(&self.lua, thread)?;
        let quantum = self.limits.instruction_budget.get().min(100) as u32;
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
    fn resume_inner(&mut self, arguments: MultiValue) -> Result<RunOutcome, VmError> {
        #[cfg(feature = "debug")]
        self.debugger.invalidate()?;
        let thread = self
            .pending
            .as_ref()
            .ok_or_else(|| VmError::new(ErrorKind::InvalidArgument, "no suspended callback"))?;
        let result = thread.resume::<MultiValue>(arguments);
        if self.control.borrow().exhausted {
            self.failed = true;
            self.pending = None;
            return Err(VmError::new(
                ErrorKind::Limit,
                "instruction budget exceeded",
            ));
        }
        match result {
            Err(error) => {
                self.failed = true;
                self.pending = None;
                Err(error.into())
            }
            Ok(_) => {
                if thread.status() == ThreadStatus::Resumable {
                    Ok(RunOutcome::Suspended)
                } else {
                    self.pending = None;
                    Ok(RunOutcome::Completed)
                }
            }
        }
    }
    /// 残りの命令数バジェットを維持したまま、同一の中断中の継続を再開します。
    #[cfg(feature = "debug")]
    pub fn resume(&mut self, mode: StepMode) -> Result<RunOutcome, VmError> {
        if self.pending.is_none() {
            return Err(VmError::new(
                ErrorKind::InvalidArgument,
                "no suspended callback",
            ));
        }
        {
            let mut state = self.control.borrow_mut();
            state.target_depth = state.depth;
            state.step = Some(mode);
            state.skip_first_line = true;
        }
        // ウォッチ式の評価が別のコルーチンを使用した可能性があるため、mluaの単一フックを復元します。
        let thread = self
            .pending
            .as_ref()
            .ok_or_else(|| VmError::new(ErrorKind::InvalidArgument, "no callback"))?;
        if self.debugger.hook_dirty {
            {
                let mut state = self.control.borrow_mut();
                state.remaining = state.remaining.saturating_sub(100);
            }
            self.install_hook(thread)?;
            self.debugger.hook_dirty = false;
        }
        self.resume_inner(MultiValue::new())
    }
    /// ソース名／行のブレークポイントを設定します。空のソース名はすべてのチャンクに一致します。
    #[cfg(feature = "debug")]
    pub fn set_breakpoints(&mut self, breakpoints: Vec<(String, i32)>) -> Result<(), VmError> {
        if breakpoints.len() > 4096
            || breakpoints
                .iter()
                .any(|(name, line)| *line < 1 || name.len() > 1024)
        {
            return Err(VmError::new(
                ErrorKind::InvalidArgument,
                "invalid breakpoint list",
            ));
        }
        self.control.borrow_mut().breakpoints = breakpoints;
        Ok(())
    }
}
