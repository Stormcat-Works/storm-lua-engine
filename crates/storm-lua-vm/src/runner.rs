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

mod synchronous;
pub use synchronous::{SynchronousCallbackPass, SynchronousCallbacks};

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
/// Luaエラーが起きた段階。どちらも`ErrorKind::Lua`です。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LuaErrorStage {
    /// チャンクのコンパイル中。ユーザーコードはまだ実行されていません。
    Syntax,
    /// コンパイル済みコードの実行中。
    Runtime,
}
/// エラー時の呼び出し履歴の1フレーム。エラーを起こしたフレームが0です。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ErrorFrame {
    /// スタック深度インデックス。
    pub level: u32,
    /// 省略されていないチャンク名。Cフレームでは`=[C]`です。
    pub source: String,
    /// 実行中の行番号（Cフレームの場合は -1）。
    pub line: i32,
    /// 取得可能な関数名。不明な場合と、UTF-8でない名前の場合はNone。
    pub function_name: Option<String>,
}
/// エラーの位置情報。チャンク名と呼び出し履歴は、メッセージ文字列ではなくスタックから取得します。
/// 構文エラーの行だけは、Luaがメッセージの先頭に付ける行番号を読みます。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ErrorTrace {
    /// `ErrorKind::Lua`のときの段階。それ以外のカテゴリではNone。
    pub stage: Option<LuaErrorStage>,
    /// エラーを起こした最も近いLuaフレーム、または構文エラーの行。取得できない場合はNone。
    /// `error(message, 2)`のようにメッセージが別の階層を指す場合でも、送出した位置を示します。
    pub location: Option<crate::logging::LogLocation>,
    /// 最大256件の呼び出し履歴。構文エラーでは空です。
    pub stack: Vec<ErrorFrame>,
}
/// 診断テキストを伴う明示的なエンジンエラー。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VmError {
    /// 機械可読なカテゴリ。
    pub kind: ErrorKind,
    /// 診断の詳細情報（空の成功に置換されることはありません）。
    pub message: String,
    /// スクリプトの読み込み・実行で失敗したときの位置情報。ホスト引数の誤りなどではNone。
    pub trace: Option<Box<ErrorTrace>>,
}
impl VmError {
    /// ホストドメインのエラーを構築します。
    pub fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
            trace: None,
        }
    }
}
/// 入れ子のコールバックエラーをたどり、Luaの構文エラーが原因かどうかを判定します。
pub(crate) fn syntax_message(error: &mlua::Error) -> Option<&str> {
    match error {
        mlua::Error::SyntaxError { message, .. } => Some(message),
        mlua::Error::CallbackError { cause, .. } => syntax_message(cause),
        _ => None,
    }
}
/// Lua 5.3の`luaO_chunkid`と同じ規則で、メッセージ中のチャンク表記を求めます。
fn chunk_id(name: &[u8]) -> Vec<u8> {
    const SIZE: usize = 60;
    let length = name.len();
    match name.first() {
        Some(b'=') if length <= SIZE => name[1..].to_vec(),
        Some(b'=') => name[1..SIZE].to_vec(),
        Some(b'@') if length <= SIZE => name[1..].to_vec(),
        Some(b'@') => {
            let keep = SIZE - 3;
            let mut out = b"...".to_vec();
            out.extend_from_slice(&name[1 + length - keep..]);
            out
        }
        _ => {
            let room = SIZE - b"[string \"...\"]".len() - 1;
            let newline = name.iter().position(|byte| *byte == b'\n');
            let mut out = b"[string \"".to_vec();
            if length < room && newline.is_none() {
                out.extend_from_slice(name);
            } else {
                out.extend_from_slice(&name[..newline.unwrap_or(length).min(room)]);
                out.extend_from_slice(b"...");
            }
            out.extend_from_slice(b"\"]");
            out
        }
    }
}
/// 構文エラーの行番号。チャンク名はエンジンが渡した値を使い、メッセージからは
/// Luaが付けた`チャンク表記:行:`の行だけを読みます。形式が一致しなければ推測しません。
pub(crate) fn syntax_location(message: &str, name: &str) -> Option<crate::logging::LogLocation> {
    let id = chunk_id(name.as_bytes());
    let rest = message
        .strip_prefix(String::from_utf8_lossy(&id).as_ref())?
        .strip_prefix(':')?;
    let digits = rest.split(':').next()?;
    let line = digits.parse::<u32>().ok().filter(|line| *line > 0)?;
    Some(crate::logging::LogLocation {
        chunk: name.to_owned(),
        line,
    })
}
/// 実行していないコルーチンから位置情報を読み、エラーへ付けます。
/// 読み取りに失敗した場合は位置なしとし、元のエラーを別のエラーで置き換えません。
pub(crate) fn attach_trace(
    lua: &Lua,
    thread: &Thread,
    error: &mut VmError,
    syntax_site: Option<crate::logging::LogLocation>,
    hook_line: Option<i32>,
) {
    // フックからyieldして止まったコルーチンだけが再開可能な状態で残ります。
    let hook_yielded = thread.status() == ThreadStatus::Resumable;
    let stack: Vec<ErrorFrame> =
        crate::backend_support::thread_frames(lua, thread, 256, hook_yielded)
            .unwrap_or_default()
            .into_iter()
            .enumerate()
            .map(|(level, frame)| ErrorFrame {
                level: level as u32,
                source: frame.source,
                line: match (hook_yielded && level == 0, frame.is_lua) {
                    (true, true) => hook_line.unwrap_or(-1),
                    (false, true) => frame.line,
                    (_, false) => -1,
                },
                function_name: frame.name,
            })
            .collect();
    let syntax = error
        .trace
        .as_ref()
        .is_some_and(|trace| trace.stage == Some(LuaErrorStage::Syntax));
    let location = if syntax {
        syntax_site
    } else {
        stack.iter().find_map(|frame| {
            let line = u32::try_from(frame.line).ok().filter(|line| *line > 0)?;
            Some(crate::logging::LogLocation {
                chunk: frame.source.clone(),
                line,
            })
        })
    };
    error.trace = Some(Box::new(ErrorTrace {
        stage: match error.kind {
            ErrorKind::Lua if syntax => Some(LuaErrorStage::Syntax),
            ErrorKind::Lua => Some(LuaErrorStage::Runtime),
            _ => None,
        },
        location,
        stack,
    }));
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
        let mut error = Self::new(category(&value), value.to_string());
        if error.kind == ErrorKind::Lua && syntax_message(&value).is_some() {
            error.trace = Some(Box::new(ErrorTrace {
                stage: Some(LuaErrorStage::Syntax),
                ..ErrorTrace::default()
            }));
        }
        error
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
/// 実行中のスクリプトを止めるかどうかを、ホストが答える関数。
///
/// VMは[`PAUSE_POLL_INTERVAL`]命令ごとに呼びます。trueを返すと、次にyieldできる命令で
/// コールバックを中断します。VMやLuaの値へはアクセスできません。エラーを返すと、
/// 実行中のスクリプトへそのエラーを送出します（続行にも停止にも読み替えません）。
#[cfg(feature = "debug")]
pub type PausePoll = Rc<dyn Fn() -> Result<bool, VmError>>;
/// [`PausePoll`]を呼ぶ間隔（Lua VMの命令数）。
#[cfg(feature = "debug")]
pub const PAUSE_POLL_INTERVAL: u32 = 4096;

#[derive(Default)]
pub(crate) struct HookControl {
    pub(crate) remaining: u64,
    pub(crate) exhausted: bool,
    /// 命令数の上限でyieldしたときに、フックが報告した実行行。
    pub(crate) limit_line: Option<i32>,
    /// 現在の入口の壁時計期限。時間制限が無効な入口ではNoneです。
    pub(crate) deadline: Option<std::time::Instant>,
    /// 時間制限を超過したかどうか。フックが記録し、入口側が実ゲーム文面のLuaエラーへ変換します。
    pub(crate) time_exceeded: bool,
    /// 次に時計を確認するまでの命令数。
    pub(crate) time_countdown: u32,
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
    /// 次に実行する行で止まる予約。入口をまたいで保持し、止まった時点で消します。
    #[cfg(feature = "debug")]
    pub(crate) pause_requested: bool,
    /// ステップが入口の終わりを越えたとき、予約へ引き継ぐかどうか。
    #[cfg(feature = "debug")]
    pub(crate) carry_step: bool,
    #[cfg(feature = "debug")]
    pub(crate) pause_poll: Option<PausePoll>,
    #[cfg(feature = "debug")]
    pub(crate) poll_countdown: u32,
    /// pollがtrueを返したが、yieldできない場所にいた。次にyieldできる命令で止まります。
    #[cfg(feature = "debug")]
    pub(crate) poll_pending: bool,
}

/// フックが時計を確認する間隔（命令数）。1001msの制限に対して検出遅れは無視できる範囲です。
pub(crate) const TIME_CHECK_INTERVAL: u32 = 512;
impl HookControl {
    /// 入口の開始時に制限カウンタを初期化します。入れ子の呼び出しでは呼びません
    /// （制限は入口単位であり、内側の呼び出しが延長することはありません）。
    pub(crate) fn begin_entry(&mut self, limits: &ExecutionLimits) {
        self.remaining = limits.instruction_allowance();
        self.exhausted = false;
        self.limit_line = None;
        self.time_exceeded = false;
        self.time_countdown = TIME_CHECK_INTERVAL;
        self.deadline = limits
            .time_limit
            .map(|limit| std::time::Instant::now() + limit);
    }
    /// フックのカウントイベントごとに呼び、時間制限の超過を記録します。
    pub(crate) fn note_instructions(&mut self, quantum: u32) {
        if self.deadline.is_none() || self.time_exceeded {
            return;
        }
        self.time_countdown = self.time_countdown.saturating_sub(quantum);
        if self.time_countdown == 0 {
            self.time_countdown = TIME_CHECK_INTERVAL;
            if self
                .deadline
                .is_some_and(|deadline| std::time::Instant::now() >= deadline)
            {
                self.time_exceeded = true;
            }
        }
    }
}

/// 1つのスレッド拘束型Lua状態。ホストコードがすべての呼び出しと継続を明示的に駆動します。
pub struct Vm {
    pub(crate) lua: Lua,
    pub(crate) environment: Table,
    pub(crate) pending: Option<Thread>,
    pub(crate) control: Rc<RefCell<HookControl>>,
    pub(crate) limits: ExecutionLimits,
    failed: Rc<std::cell::Cell<bool>>,
    synchronous: Rc<synchronous::Context>,
    _synchronous_owner: Rc<()>,
    pub(crate) environment_profile: storm_lua_spec::environment::EnvironmentProfile,
    pub(crate) logs: Rc<RefCell<crate::logging::LogBuffer>>,
    /// requireが読んだチャンクの構文エラー位置。実行中のエラーへ付けるまで保持します。
    pub(crate) syntax_site: Rc<RefCell<Option<crate::logging::LogLocation>>>,
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
        if let Some(memory) = limits.lua_memory_bytes {
            if lua.used_memory() > memory.get() {
                return Err(VmError::new(
                    ErrorKind::Limit,
                    "Lua memory budget is smaller than initialization",
                ));
            }
            lua.set_memory_limit(memory.get())?;
        }
        let environment = crate::sandbox::environment(&lua, environment_profile)?;
        let control = Rc::new(RefCell::new(HookControl::default()));
        if environment_profile == storm_lua_spec::environment::EnvironmentProfile::Extended {
            crate::sandbox::protect_calls(&lua, &environment, Rc::clone(&control), limits)?;
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
        let synchronous_owner = Rc::new(());
        let synchronous = synchronous::Context::new(
            &lua,
            &environment,
            Rc::clone(&control),
            limits,
            Rc::downgrade(&synchronous_owner),
        );
        let failed = Rc::clone(&synchronous.failed);
        Ok(Self {
            lua,
            environment,
            pending: None,
            control,
            limits,
            failed,
            synchronous,
            _synchronous_owner: synchronous_owner,
            environment_profile,
            logs,
            syntax_site: Rc::default(),
            #[cfg(feature = "debug")]
            debugger,
        })
    }
    /// Addon環境のVMです。通常のLuaエラーでVMが失敗状態にならない点と、
    /// 関数でないコールバックを不在として扱う点は全プロファイル共通になりました。
    /// Addon固有の差は同期ホストイベント側にあり、そちらの通常のLuaエラーは
    /// 呼び出し元へ戻して回復可能にします（制限は失敗状態にします）。
    pub fn with_addon_environment(
        limits: ExecutionLimits,
        environment_profile: storm_lua_spec::environment::EnvironmentProfile,
    ) -> Result<Self, VmError> {
        let vm = Self::with_environment(limits, environment_profile)?;
        vm.synchronous.addon_callbacks.set(true);
        Ok(vm)
    }
    /// Component Mod環境のVMです。Addonと同じ保持モード（通常のLuaエラーで
    /// VMが失敗状態にならず、関数でないコールバックを不在として扱う）を
    /// 使います。Component固有のライフサイクルとエラーステータスの管理は
    /// storm-lua-component側が所有します。
    pub fn with_component_environment(
        limits: ExecutionLimits,
        environment_profile: storm_lua_spec::environment::EnvironmentProfile,
    ) -> Result<Self, VmError> {
        let vm = Self::with_environment(limits, environment_profile)?;
        vm.synchronous.addon_callbacks.set(true);
        Ok(vm)
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
    /// Gameプロファイルの既定（CRT互換の共有ストリーム）を、VMごとの決定的
    /// ストリームへ差し替えます（ホスト選択肢、J2）。Extendedではもともと
    /// 決定的ストリームのため実質的に変わりません。
    pub fn use_deterministic_random(&mut self) -> Result<(), VmError> {
        self.ensure_idle()?;
        crate::sandbox::install_random(&self.lua, &self.environment.get::<Table>("math")?)?;
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
        if self.failed.get() {
            return Err(VmError::new(
                ErrorKind::Failed,
                "reset the VM after failure",
            ));
        }
        if self.pending.is_some() || self.synchronous.active.get() != 0 {
            return Err(VmError::new(
                ErrorKind::Busy,
                "callback is active or suspended",
            ));
        }
        Ok(())
    }
    /// コールバックの継続が保持されている（中断中である）かどうか。
    pub fn is_suspended(&self) -> bool {
        self.pending.is_some()
    }
    /// 実行が失敗したかどうか。リセットするにはホストがこのインスタンスを再作成する必要があります。
    pub fn is_failed(&self) -> bool {
        self.failed.get()
    }
    /// ホストバッファを除いた現在のLuaヒープ使用量。
    pub fn used_memory(&self) -> usize {
        self.lua.used_memory()
    }
    /// テキストのみをコンパイルし、ホストバインディング組み込み後にそのトップレベルを実行します。
    pub fn execute(&mut self, source: &[u8], name: &str) -> Result<RunOutcome, VmError> {
        self.ensure_idle()?;
        crate::source::validate_chunk_name(name)?;
        self.check_source_len(source.len())?;
        let source = crate::dialect::rewrite(source)
            .map_err(|message| VmError::new(ErrorKind::Lua, message))?;
        self.check_source_len(source.len())?;
        let function = self
            .lua
            .load(source.as_ref())
            .set_name(name)
            .set_mode(ChunkMode::Text)
            .set_environment(self.environment.clone())
            .into_function()
            .map_err(|error| {
                let location =
                    syntax_message(&error).and_then(|message| syntax_location(message, name));
                let mut error = VmError::from(error);
                if let Some(trace) = &mut error.trace {
                    trace.location = location;
                }
                error
            })?;
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
            // 実ゲームの共通呼び出し（call_lua_function）は、対象が関数でなければ
            // 何も実行せずエラーも記録しません。全プロファイルで「不在」として扱います。
            _ => Ok(RunOutcome::Missing),
        }
    }
    /// ソース長制限はホストが有効化した場合だけ適用します（実ゲームにハード制限はありません）。
    fn check_source_len(&self, len: usize) -> Result<(), VmError> {
        if let Some(max) = self.limits.max_source_bytes {
            if len > max.get() {
                return Err(VmError::new(
                    ErrorKind::Limit,
                    "Lua source exceeds the host source limit",
                ));
            }
        }
        Ok(())
    }
    fn start(&mut self, function: Function, arguments: MultiValue) -> Result<RunOutcome, VmError> {
        self.ensure_idle()?;
        {
            let mut control = self.control.borrow_mut();
            control.begin_entry(&self.limits);
            #[cfg(feature = "debug")]
            {
                control.depth = 0;
                control.step = None;
                control.carry_step = false;
                control.skip_first_line = false;
                control.poll_countdown = PAUSE_POLL_INTERVAL;
                control.poll_pending = false;
            }
        }
        let thread = self.lua.create_thread(function)?;
        self.install_hook(&thread)?;
        self.pending = Some(thread);
        self.synchronous.main_pending.set(true);
        self.resume_inner(arguments)
    }
    pub(crate) fn install_hook(&self, thread: &Thread) -> Result<(), VmError> {
        self.synchronous.install_main_hook(thread)
    }
    fn resume_inner(&mut self, arguments: MultiValue) -> Result<RunOutcome, VmError> {
        #[cfg(feature = "debug")]
        self.debugger.invalidate()?;
        let thread = self
            .pending
            .clone()
            .ok_or_else(|| VmError::new(ErrorKind::InvalidArgument, "no suspended callback"))?;
        let thread = &thread;
        self.syntax_site.borrow_mut().take();
        let result = {
            let _active = synchronous::ActiveCall::enter(&self.synchronous, thread, true);
            thread.resume::<MultiValue>(arguments)
        };
        if let Some(error) = self.synchronous.restore_error.borrow_mut().take() {
            self.failed.set(true);
            self.pending = None;
            self.synchronous.main_pending.set(false);
            return Err(error);
        }
        if self.control.borrow().time_exceeded {
            // 時間制限の超過は実ゲームでは通常のLuaエラーであり、VMは失敗状態になりません。
            self.pending = None;
            self.synchronous.main_pending.set(false);
            let line = {
                let mut control = self.control.borrow_mut();
                control.time_exceeded = false;
                control.deadline = None;
                control.limit_line.take()
            };
            let mut error = VmError::new(ErrorKind::Lua, self.limits.timeout_message(line));
            attach_trace(&self.lua, thread, &mut error, None, line);
            return Err(error);
        }
        if self.control.borrow().exhausted {
            self.failed.set(true);
            self.pending = None;
            self.synchronous.main_pending.set(false);
            let mut error = VmError::new(ErrorKind::Limit, "instruction budget exceeded");
            let line = self.control.borrow_mut().limit_line.take();
            attach_trace(&self.lua, thread, &mut error, None, line);
            return Err(error);
        }
        match result {
            Err(error) => {
                let mut error = VmError::from(error);
                let site = self.syntax_site.borrow_mut().take();
                attach_trace(&self.lua, thread, &mut error, site, None);
                // 通常のLuaエラーではVMは失敗状態になりません（実ゲームのエラーステータス
                // モデル: エラー後もVMは生存し、クラス側がステータスで管理します）。
                // 制限・ホスト等の異常だけが従来どおり失敗状態にします。
                self.failed
                    .set(self.failed.get() || error.kind != ErrorKind::Lua);
                self.pending = None;
                self.synchronous.main_pending.set(false);
                Err(error)
            }
            Ok(_) => {
                if thread.status() == ThreadStatus::Resumable {
                    Ok(RunOutcome::Suspended)
                } else {
                    self.pending = None;
                    self.synchronous.main_pending.set(false);
                    #[cfg(feature = "debug")]
                    {
                        // 入口の終わりを越えたステップは、指定があれば次の入口の最初の行へ引き継ぎます。
                        let mut control = self.control.borrow_mut();
                        let stepping = matches!(
                            control.step,
                            Some(StepMode::Into | StepMode::Over | StepMode::Out)
                        );
                        if stepping && control.carry_step {
                            control.pause_requested = true;
                        }
                        control.step = None;
                        control.carry_step = false;
                    }
                    Ok(RunOutcome::Completed)
                }
            }
        }
    }
    /// 次に実行する行で止まるよう予約します。実行していないとき（アイドルまたは中断中）に呼びます。
    ///
    /// アイドル中に呼ぶと、次に実行する入口の最初の行で中断します。中断中に呼ぶと、
    /// 再開後の次の行で中断します。予約は止まるまで残り、存在しないコールバックの
    /// 呼び出しでは消えません。実行中のスクリプトを止めるには[`Vm::set_pause_poll`]を使います。
    #[cfg(feature = "debug")]
    pub fn request_pause(&mut self) -> Result<(), VmError> {
        if self.failed.get() {
            return Err(VmError::new(ErrorKind::Failed, "VM has failed"));
        }
        self.control.borrow_mut().pause_requested = true;
        Ok(())
    }
    /// 止まっていない一時停止の予約を取り消します。予約がなければ何もしません。
    #[cfg(feature = "debug")]
    pub fn cancel_pause(&mut self) {
        self.control.borrow_mut().pause_requested = false;
    }
    /// 実行中の一時停止を問い合わせる関数を設定または解除します。アイドル中に呼びます。
    #[cfg(feature = "debug")]
    pub fn set_pause_poll(&mut self, poll: Option<PausePoll>) -> Result<(), VmError> {
        self.ensure_idle()?;
        self.control.borrow_mut().pause_poll = poll;
        Ok(())
    }
    /// 残りの命令数バジェットを維持したまま、同一の中断中の継続を再開します。
    #[cfg(feature = "debug")]
    pub fn resume(&mut self, mode: StepMode) -> Result<RunOutcome, VmError> {
        self.resume_with(mode, false)
    }
    /// [`Vm::resume`]と同じですが、`carry`がtrueのとき、ステップが入口の終わりを越えると
    /// 次に実行する入口の最初の行で止まります。`StepMode::Continue`には影響しません。
    #[cfg(feature = "debug")]
    pub fn resume_with(&mut self, mode: StepMode, carry: bool) -> Result<RunOutcome, VmError> {
        if self.failed.get() {
            return Err(VmError::new(ErrorKind::Failed, "VM has failed"));
        }
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
            state.carry_step = carry;
            state.skip_first_line = true;
        }
        // ウォッチ式の評価が別のコルーチンを使用した可能性があるため、mluaの単一フックを復元します。
        let thread = self
            .pending
            .as_ref()
            .ok_or_else(|| VmError::new(ErrorKind::InvalidArgument, "no callback"))?;
        if self.debugger.hook_dirty {
            let mut state = self.control.borrow_mut();
            state.remaining = state.remaining.saturating_sub(100);
        }
        // 行フックは必要なときだけ付けます。行フックなしで始めた実行がpollで止まった後でも
        // ステップや後から置いたブレークポイントが働くよう、再開のたびにフックを付け直します。
        self.install_hook(thread)?;
        self.debugger.hook_dirty = false;
        self.resume_inner(MultiValue::new())
    }
    #[cfg(feature = "debug")]
    pub(crate) fn evaluate_watch_thread(&mut self, evaluator: &Thread) -> Result<Value, VmError> {
        let previous = {
            let mut control = self.control.borrow_mut();
            let previous = (
                control.remaining,
                control.exhausted,
                control.time_exceeded,
                control.deadline,
            );
            control.remaining = 100_000;
            control.exhausted = false;
            control.time_exceeded = false;
            control.deadline = None;
            previous
        };
        self.synchronous.install_synchronous_hook(evaluator);
        let result = {
            let _active = synchronous::ActiveCall::enter(&self.synchronous, evaluator, false);
            evaluator.resume::<Value>(())
        };
        let exhausted = {
            let mut control = self.control.borrow_mut();
            let exhausted = control.exhausted;
            (
                control.remaining,
                control.exhausted,
                control.time_exceeded,
                control.deadline,
            ) = previous;
            exhausted
        };
        if let Some(error) = self.synchronous.restore_error.borrow_mut().take() {
            self.failed.set(true);
            return Err(error);
        }
        if exhausted {
            return Err(VmError::new(
                ErrorKind::Limit,
                "watch instruction budget exceeded",
            ));
        }
        Ok(result?)
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
