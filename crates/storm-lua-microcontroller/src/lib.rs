//! CPUラスタライズから独立したマイクロコントローラーLua APIおよびコールバックのライフサイクル。
mod bindings;
mod controls;
#[cfg(feature = "debug")]
mod debugger;
use std::{
    ops::Deref,
    sync::{
        atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicU8, Ordering},
        Arc, PoisonError, RwLock, RwLockReadGuard, RwLockWriteGuard,
    },
};
use storm_lua_spec::{
    draw::{CommandBuffer, DrawCommand, ScreenSink},
    io::CompositeSignal,
    property::{PropertyBag, PropertyValue},
};
/// VM所有者と共有されるコールバック完了状態。
pub use storm_lua_vm::runner::RunOutcome as CallbackOutcome;
use storm_lua_vm::{
    runner::{ErrorKind, RandomStreamState, RunOutcome, Vm, VmError},
    ExecutionLimits,
};

/// トップレベルコードが実行される前にプロパティが組み込まれます。
#[derive(Clone, Default)]
pub struct MicrocontrollerConfig {
    /// 大文字小文字を区別する型付きプロパティ値（UIウィジェットのメタデータは含みません）。
    pub properties: PropertyBag,
    /// Luaの命令数およびヒープメモリの上限。
    pub limits: ExecutionLimits,
    /// Script-visible game or explicitly extended environment.
    pub environment: storm_lua_spec::environment::EnvironmentProfile,
    /// Explicit host extensions, installed before load and preserved on reset.
    pub bindings: storm_lua_vm::bindings::HostBindings,
    /// Development include-once require, explicit and available only in extended.
    pub require_loader: Option<storm_lua_vm::source::RequireLoader>,
    /// Optional native state-control namespace for an extended development harness.
    /// Never installed in the game environment or selected implicitly.
    pub control_namespace: Option<String>,
    /// ホスト共有のCRT乱数コンテキスト。指定した場合は `deterministic_random` より優先します。
    /// 未指定時のGameプロファイル既定は、エンジン組み込みの共有CRT互換ストリームです（J2）。
    pub random: Option<storm_lua_vm::random::SharedCrtRandom>,
    /// trueの場合、Gameプロファイルの既定（共有CRT互換ストリーム）ではなく
    /// VMごとの決定的ストリームを使います（J2のホスト選択肢）。
    pub deterministic_random: bool,
}
impl std::fmt::Debug for MicrocontrollerConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MicrocontrollerConfig")
            .field("properties", &self.properties)
            .field("limits", &self.limits)
            .field("environment", &self.environment)
            .field("bindings", &self.bindings)
            .field("require_loader", &self.require_loader)
            .field("control_namespace", &self.control_namespace)
            .field(
                "random",
                &self.random.as_ref().map(|_| "<host CRT context>"),
            )
            .field("deterministic_random", &self.deterministic_random)
            .finish()
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub(crate) enum Phase {
    Idle = 0,
    Init = 1,
    Tick = 2,
    Draw = 3,
}
impl Phase {
    #[inline]
    fn from_raw(raw: u8) -> Self {
        match raw {
            1 => Self::Init,
            2 => Self::Tick,
            3 => Self::Draw,
            _ => Self::Idle,
        }
    }
}

/// ホットパス（毎tick/毎drawのフェーズ判定・画面サイズ・Composite I/O）向けのロックフリー状態。
pub(crate) struct FastIo {
    phase: AtomicU8,
    size: AtomicU64,
    pub(crate) map_used: AtomicBool,
    pub(crate) error_status: AtomicU32,
    pub(crate) entry_error_status: AtomicU32,
    pub(crate) input_numbers: [AtomicU32; 32],
    pub(crate) input_booleans: [AtomicBool; 32],
    pub(crate) input_changed: AtomicBool,
    pub(crate) output_numbers: [AtomicU32; 32],
    pub(crate) output_booleans: [AtomicBool; 32],
}
impl FastIo {
    fn new() -> Self {
        Self {
            phase: AtomicU8::new(Phase::Idle as u8),
            size: AtomicU64::new(0),
            map_used: AtomicBool::new(false),
            error_status: AtomicU32::new(0),
            entry_error_status: AtomicU32::new(3),
            input_numbers: std::array::from_fn(|_| AtomicU32::new(0)),
            input_booleans: std::array::from_fn(|_| AtomicBool::new(false)),
            input_changed: AtomicBool::new(false),
            output_numbers: std::array::from_fn(|_| AtomicU32::new(0)),
            output_booleans: std::array::from_fn(|_| AtomicBool::new(false)),
        }
    }
    #[inline]
    pub(crate) fn phase(&self) -> Phase {
        Phase::from_raw(self.phase.load(Ordering::Relaxed))
    }
    #[inline]
    pub(crate) fn set_phase(&self, phase: Phase) {
        self.phase.store(phase as u8, Ordering::Relaxed);
    }
    #[inline]
    pub(crate) fn size(&self) -> (u32, u32) {
        let packed = self.size.load(Ordering::Relaxed);
        (packed as u32, (packed >> 32) as u32)
    }
    #[inline]
    pub(crate) fn set_size(&self, width: u32, height: u32) {
        let packed = u64::from(width) | (u64::from(height) << 32);
        self.size.store(packed, Ordering::Relaxed);
    }
    #[inline]
    pub(crate) fn store_input(&self, input: &CompositeSignal) {
        for (slot, &val) in self.input_numbers.iter().zip(&input.numbers) {
            slot.store(val.to_bits(), Ordering::Relaxed);
        }
        for (slot, &val) in self.input_booleans.iter().zip(&input.booleans) {
            slot.store(val, Ordering::Relaxed);
        }
    }
    #[inline]
    pub(crate) fn load_input(&self) -> CompositeSignal {
        let mut numbers = [0.0_f32; 32];
        let mut booleans = [false; 32];
        for (dst, slot) in numbers.iter_mut().zip(&self.input_numbers) {
            *dst = f32::from_bits(slot.load(Ordering::Relaxed));
        }
        for (dst, slot) in booleans.iter_mut().zip(&self.input_booleans) {
            *dst = slot.load(Ordering::Relaxed);
        }
        CompositeSignal { numbers, booleans }
    }
    #[inline]
    pub(crate) fn store_output(&self, output: &CompositeSignal) {
        for (slot, &val) in self.output_numbers.iter().zip(&output.numbers) {
            slot.store(val.to_bits(), Ordering::Relaxed);
        }
        for (slot, &val) in self.output_booleans.iter().zip(&output.booleans) {
            slot.store(val, Ordering::Relaxed);
        }
    }
    #[inline]
    pub(crate) fn load_output(&self) -> CompositeSignal {
        let mut numbers = [0.0_f32; 32];
        let mut booleans = [false; 32];
        for (dst, slot) in numbers.iter_mut().zip(&self.output_numbers) {
            *dst = f32::from_bits(slot.load(Ordering::Relaxed));
        }
        for (dst, slot) in booleans.iter_mut().zip(&self.output_booleans) {
            *dst = slot.load(Ordering::Relaxed);
        }
        CompositeSignal { numbers, booleans }
    }
}

/// アイドル境界で運ぶマイクロコントローラーのエラーステータス・リング継続状態（E9）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MicrocontrollerErrorContinuation {
    /// 実ゲームのエラーステータス（0=正常、1=onDraw失敗、2=onTick失敗/時間超過、3=ロード失敗、5=httpReply失敗）。
    pub status: u32,
    /// エラーリング（最大11件、古い順）。
    pub log: Vec<String>,
}

pub(crate) struct State {
    properties: PropertyBag,
    http: storm_lua_spec::http::HttpQueue,
    commands: CommandBuffer,
    /// エラーリング（実ゲームのhandle_errorと同じく最大11件、古い順）。
    errors: Vec<String>,
}
/// 明示的なtick/draw駆動と再利用可能なコマンドストレージを備えたスレッド拘束型VM。
pub struct Microcontroller {
    vm: Vm,
    io: Arc<FastIo>,
    state: Arc<RwLock<State>>,
    limits: ExecutionLimits,
    random: Option<storm_lua_vm::random::SharedCrtRandom>,
    deterministic_random: bool,
    sources: Vec<storm_lua_vm::source::SourceChunk>,
    source_bytes: usize,
    pending_source: Option<storm_lua_vm::source::SourceChunk>,
    require_loader: Option<storm_lua_vm::source::RequireLoader>,
    dev_logs: bool,
    environment: storm_lua_spec::environment::EnvironmentProfile,
    bindings: storm_lua_vm::bindings::HostBindings,
    control_namespace: Option<String>,
    #[cfg(feature = "debug")]
    pause_poll: Option<storm_lua_vm::runner::PausePoll>,
}
impl Microcontroller {
    /// 未開始のコントローラーを作成します。loadを呼び出す前にホストオプションを設定してください。
    pub fn new(config: MicrocontrollerConfig) -> Result<Self, VmError> {
        config.bindings.validate(config.environment)?;
        if let Some(loader) = &config.require_loader {
            loader.validate_configuration(config.environment, &config.bindings)?;
        }
        let mut vm = Vm::with_environment(config.limits, config.environment)?;
        if let Some(random) = &config.random {
            vm.install_crt_random(Arc::clone(random))?;
        } else if config.deterministic_random {
            vm.use_deterministic_random()?;
        }
        let io = Arc::new(FastIo::new());
        let state = Arc::new(RwLock::new(State {
            properties: config.properties,
            http: storm_lua_spec::http::HttpQueue::new().map_err(http_error)?,
            commands: CommandBuffer::new(65536, 1024 * 1024),
            errors: Vec::new(),
        }));
        vm.configure(|lua, env| bindings::install(lua, env, Arc::clone(&io), Arc::clone(&state)))?;
        let bindings = controls::with_controls(
            &config.bindings,
            config.control_namespace.as_deref(),
            config.environment,
            &io,
            &state,
        )?;
        if let Some(loader) = &config.require_loader {
            loader.validate_configuration(config.environment, &bindings)?;
        }
        if let Some(namespace) = &config.control_namespace {
            vm.configure(|_, env| {
                use storm_lua_vm::backend::{BackendError, Value};
                if !matches!(env.raw_get::<Value>(namespace.as_str())?, Value::Nil) {
                    return Err(BackendError::external(VmError::new(
                        ErrorKind::InvalidArgument,
                        "controlNamespace must not replace an existing builtin",
                    )));
                }
                Ok(())
            })?;
        }
        vm.install_bindings(&bindings)?;
        // These namespaces are installed after Vm's initial host registry seed, so
        // snapshot/restore cannot resolve them until each path is registered.
        // Register the tables and their direct entries so captured aliases rebind
        // to this unit's live host functions on restore.
        for namespace in ["input", "output", "property", "screen", "async", "map"] {
            vm.register_host_path(&[namespace])?;
            let mut names = Vec::new();
            vm.configure(|_, env| {
                let table: storm_lua_vm::backend::Table = env.raw_get(namespace)?;
                for pair in table.pairs::<String, storm_lua_vm::backend::Value>() {
                    let (name, _) = pair?;
                    names.push(name);
                }
                Ok(())
            })?;
            names.sort();
            for name in names {
                vm.register_host_path(&[namespace, &name])?;
            }
        }
        if let Some(loader) = &config.require_loader {
            vm.install_require_loader(loader)?;
        }
        Ok(Self {
            vm,
            io,
            state,
            limits: config.limits,
            random: config.random,
            deterministic_random: config.deterministic_random,
            sources: Vec::new(),
            source_bytes: 0,
            pending_source: None,
            require_loader: config.require_loader,
            dev_logs: false,
            environment: config.environment,
            bindings: config.bindings,
            control_namespace: config.control_namespace,
            #[cfg(feature = "debug")]
            pause_poll: None,
        })
    }
    /// Execute another named chunk in the existing environment. Completed loads
    /// are retained in order for reset; this appends rather than replacing code.
    /// Chunk locals are distinct. A failed or abandoned suspended load is not saved.
    pub fn load(&mut self, source: &[u8], name: &str) -> Result<RunOutcome, VmError> {
        self.vm.ensure_idle()?;
        storm_lua_vm::source::validate_chunk_name(name)?;
        // ソース長制限はホストが有効化した場合だけ適用します（実ゲームにハード制限はありません）。
        // 有効化時は保持プログラム全体の上限（128チャンク／8MiB）も併せて適用します。
        if self.limits.max_source_bytes.is_some()
            && (self.sources.len() >= storm_lua_vm::source::MAX_PROGRAM_CHUNKS
                || source.len() > storm_lua_vm::source::MAX_PROGRAM_BYTES - self.source_bytes)
        {
            return Err(VmError::new(
                ErrorKind::Limit,
                "retained load sequence exceeds 128 chunks or 8 MiB",
            ));
        }
        self.pending_source = Some(storm_lua_vm::source::SourceChunk {
            source: source.to_vec(),
            name: name.to_owned(),
        });
        self.io.set_phase(Phase::Init);
        self.io.entry_error_status.store(3, Ordering::Relaxed);
        let result = self.vm.execute(source, name);
        self.finish(result)
    }
    /// 1 tickを実行します。出力チャンネルは明示的に上書きされるまで値を保持します。
    pub fn tick(&mut self, input: &CompositeSignal) -> Result<RunOutcome, VmError> {
        self.call_tick("onTick", &[], input)
    }
    /// Execute a named global with tick-phase I/O and the normal callback budget.
    /// This does not replace onTick or evaluate dynamically constructed source.
    pub fn call_tick(
        &mut self,
        name: &str,
        arguments: &[storm_lua_vm::value::LuaValue],
        input: &CompositeSignal,
    ) -> Result<RunOutcome, VmError> {
        self.vm.ensure_idle()?;
        self.io.store_input(input);
        self.io.entry_error_status.store(2, Ordering::Relaxed);
        // エラー状態の間、実ゲームはonTickをスキップします（E9）。入力の取り込みだけ行います。
        if self.io.error_status.load(Ordering::Relaxed) != 0 {
            self.io.set_phase(Phase::Idle);
            return Ok(RunOutcome::Missing);
        }
        self.io.set_phase(Phase::Tick);
        let result = self.vm.call_with(name, arguments);
        self.finish(result)
    }
    /// 1台のモニターに対してonDrawを実行します。複数回の呼び出しは意図的に同一のLua状態を共有します。
    /// ここではラスタライザを選択しません。呼び出し側がコマンドストリームを消費します。
    pub fn draw(&mut self, width: u32, height: u32) -> Result<RunOutcome, VmError> {
        self.call_draw("onDraw", &[], width, height)
    }
    /// Execute a named global with draw-phase screen dimensions and commands.
    /// Suspension/resumption preserves this same invocation and command prefix.
    pub fn call_draw(
        &mut self,
        name: &str,
        arguments: &[storm_lua_vm::value::LuaValue],
        width: u32,
        height: u32,
    ) -> Result<RunOutcome, VmError> {
        self.vm.ensure_idle()?;
        if width == 0 || height == 0 || width > 4096 || height > 4096 {
            return Err(VmError::new(
                ErrorKind::InvalidArgument,
                "invalid monitor size",
            ));
        }
        self.io.set_size(width, height);
        self.io.map_used.store(false, Ordering::Relaxed);
        self.io.entry_error_status.store(1, Ordering::Relaxed);
        write_state(&self.state).commands.clear();
        // エラー状態の間、実ゲームはonDrawをスキップします（E9）。
        if self.io.error_status.load(Ordering::Relaxed) != 0 {
            self.io.set_phase(Phase::Idle);
            return Ok(RunOutcome::Missing);
        }
        self.io.set_phase(Phase::Draw);
        let result = self.vm.call_with(name, arguments);
        self.finish(result)
    }
    fn finish(&mut self, result: Result<RunOutcome, VmError>) -> Result<RunOutcome, VmError> {
        if !matches!(result, Ok(RunOutcome::Suspended)) {
            self.io.set_phase(Phase::Idle);
            self.io.map_used.store(false, Ordering::Relaxed);
            if let Some(chunk) = self.pending_source.take() {
                if result.is_ok() {
                    self.source_bytes += chunk.source.len();
                    self.sources.push(chunk);
                }
            }
            // 通常のLuaエラーは実ゲームのエラーステータスとリングへ記録します（E9）。
            // 制限・ホスト等の異常はVM側の失敗状態で表すため、ここでは記録しません。
            if let Err(error) = &result {
                if error.kind == ErrorKind::Lua {
                    let status = self.io.entry_error_status.load(Ordering::Relaxed);
                    self.io.error_status.store(status, Ordering::Relaxed);
                    let mut state = write_state(&self.state);
                    if state.errors.len() > 10 {
                        state.errors.remove(0);
                    }
                    state.errors.push(error.message.clone());
                }
            }
        }
        result
    }
    /// Snapshot the current input, including changes made by explicit development controls.
    pub fn input(&self) -> CompositeSignal {
        self.io.load_input()
    }
    /// Drain an input update made by development controls. None leaves pending host input intact.
    pub fn take_input_changes(&mut self) -> Option<CompositeSignal> {
        if !self.io.input_changed.swap(false, Ordering::Relaxed) {
            return None;
        }
        Some(self.io.load_input())
    }
    /// ネイティブ信号の小さなスナップショットをコピーします（JSON変換は不要）。
    /// チャンネルは明示的に上書きされるまで保持されるため、continuation は
    /// この値をそのまま運びます。
    pub fn output(&self) -> CompositeSignal {
        self.io.load_output()
    }
    /// アイドル境界で保持中の出力アキュムレータを置き換えます。
    ///
    /// 「明示的に上書きされるまで保持」が意味論なので、復元した値はそのまま
    /// 次のtick/drawまで有効です。Luaコールバック実行中は拒否します。
    pub fn set_output(&mut self, output: CompositeSignal) -> Result<(), VmError> {
        self.vm.ensure_idle()?;
        self.io.store_output(&output);
        Ok(())
    }
    /// 中断中の一部のプレフィックスを含む、順序付けられた描画コマンドを参照します。
    /// 返却されたガードは呼び出し側が保持している間、コマンドバッファを読み取りロックします。
    pub fn commands(&self) -> Commands<'_> {
        Commands(read_state(&self.state))
    }
    /// ラスタライズ依存を導入することなく、保持されている描画コマンドを再生します。
    pub fn replay(
        &self,
        sink: &mut dyn ScreenSink,
    ) -> Result<(), storm_lua_spec::draw::ScreenError> {
        let commands = read_state(&self.state).commands.commands().to_vec();
        for command in &commands {
            sink.submit(command)?;
        }
        Ok(())
    }
    /// アイドル境界で実行時プロパティをすべて置換します。既存のLuaローカル変数には影響しません。
    pub fn set_properties(&mut self, properties: PropertyBag) -> Result<(), VmError> {
        self.vm.ensure_idle()?;
        if self.control_namespace.is_some() {
            controls::validate_properties(&properties)?;
        }
        write_state(&self.state).properties = properties;
        Ok(())
    }
    /// アイドル境界で実行時プロパティを1件だけ設定します。他の項目と既存のLuaローカル変数には
    /// 影響しません。同じラベルの値があれば、型が違っても置き換えます。ラベルはバイト列のまま
    /// 比較します。実行中と中断中はBusyで拒否します。削除は`remove_property`を使います。
    pub fn set_property(&mut self, label: &[u8], value: PropertyValue) -> Result<(), VmError> {
        self.vm.ensure_idle()?;
        let mut state = write_state(&self.state);
        if self.control_namespace.is_some() {
            controls::validate_property_update(&state.properties, label, &value)?;
        }
        state.properties.insert(label.to_vec(), value);
        Ok(())
    }
    /// アイドル境界で実行時プロパティを1件だけ削除し、その項目があったかどうかを返します。
    /// 存在しないラベルはエラーにせず、falseを返します。実行中と中断中はBusyで拒否します。
    pub fn remove_property(&mut self, label: &[u8]) -> Result<bool, VmError> {
        self.vm.ensure_idle()?;
        Ok(write_state(&self.state).properties.remove(label).is_some())
    }
    /// テキストデコードや数値の型縮小を行わずにホストプロパティデータを検査します。
    /// 返却されたガードは呼び出し側が保持している間、状態を読み取りロックします。
    pub fn properties(&self) -> Properties<'_> {
        Properties(read_state(&self.state))
    }
    /// extended専用のログ機能を明示要求します。debug.logはgameでも利用可能です。
    /// ホストのデバッガ設定や、明示的に置換されたprintを変更しません。
    pub fn enable_dev_logs(&mut self) -> Result<(), VmError> {
        self.vm.ensure_idle()?;
        self.vm.enable_logs()?;
        self.dev_logs = true;
        Ok(())
    }
    /// 上限付きの生ログ行を取り出します。このコールドパスは所有権を転送します。
    pub fn drain_logs(&mut self) -> Vec<Vec<u8>> {
        self.vm
            .drain_log_records()
            .into_iter()
            .map(|record| record.bytes)
            .collect()
    }
    /// print/debug.log の出所を維持しつつ、ホスト配信用の構造化レコードを取り出します。
    pub fn drain_log_records(&mut self) -> Vec<storm_lua_vm::logging::LogRecord> {
        self.vm.drain_log_records()
    }
    /// コールバックの継続が一時停止中（中断中）かどうか。
    pub fn is_suspended(&self) -> bool {
        self.vm.is_suspended()
    }
    /// 制限・ホスト等の異常でVMが失敗状態かどうか。通常のLuaエラーでは
    /// 失敗状態になりません（[`Microcontroller::error_status`]を参照）。
    pub fn is_failed(&self) -> bool {
        self.vm.is_failed()
    }
    /// 実ゲームのエラーステータス（0=正常、1=onDraw失敗、2=onTick失敗/時間超過、
    /// 3=ロード失敗、5=httpReply失敗）。エラー状態の間はonTick/onDrawが
    /// スキップされます。リセットで0に戻ります。
    pub fn error_status(&self) -> u32 {
        self.io.error_status.load(Ordering::Relaxed)
    }
    /// 記録されたエラーメッセージ（最大11件、古い順）。
    pub fn error_log(&self) -> Vec<String> {
        read_state(&self.state).errors.clone()
    }
    /// アイドル境界でエラーステータスとエラーリングを取得します（E9 continuation）。
    pub fn error_continuation(&self) -> Result<MicrocontrollerErrorContinuation, VmError> {
        self.vm.ensure_idle()?;
        Ok(MicrocontrollerErrorContinuation {
            status: self.io.error_status.load(Ordering::Relaxed),
            log: read_state(&self.state).errors.clone(),
        })
    }
    /// アイドル境界でエラーステータスとエラーリングを検証のうえ復元します（E9 continuation）。
    pub fn restore_error_continuation(
        &mut self,
        continuation: MicrocontrollerErrorContinuation,
    ) -> Result<(), VmError> {
        self.vm.ensure_idle()?;
        if !matches!(continuation.status, 0 | 1 | 2 | 3 | 5) {
            return Err(VmError::new(
                ErrorKind::InvalidArgument,
                format!(
                    "invalid microcontroller error status {}",
                    continuation.status
                ),
            ));
        }
        if continuation.log.len() > 11 {
            return Err(VmError::new(
                ErrorKind::InvalidArgument,
                "microcontroller error log exceeds 11 entries",
            ));
        }
        self.io
            .error_status
            .store(continuation.status, Ordering::Relaxed);
        write_state(&self.state).errors = continuation.log;
        Ok(())
    }
    /// アイドル境界でVMヒープ全体のスナップショットを取得します。
    ///
    /// `Vm::snapshot` の転送であり、失敗したVM・実行中コールバックのVMでは
    /// Vm側の型付きエラーがそのまま伝播します。`Vm` のヒープ外状態
    /// （乱数ストリーム・出力・エラーステータス）は含まないため、continuation は
    /// [`Microcontroller::random_stream_state`] 等も併せて運びます。
    pub fn snapshot(&self) -> Result<Vec<u8>, VmError> {
        self.vm.snapshot()
    }
    /// アイドル境界でヒープスナップショットをこのVMへ復元します。
    pub fn restore(&mut self, blob: &[u8]) -> Result<(), VmError> {
        self.vm.restore(blob)
    }
    /// アイドル境界で `math.random` ストリームの状態を返します。
    /// ホスト共有CRTストリームのVMでは [`RandomStreamState::HostOwned`] です。
    pub fn random_stream_state(&self) -> Result<RandomStreamState, VmError> {
        self.vm.random_stream_state()
    }
    /// アイドル境界で `math.random` ストリームの状態を置き換えます。
    pub fn set_random_stream_state(&mut self, state: RandomStreamState) -> Result<(), VmError> {
        self.vm.set_random_stream_state(state)
    }
    /// Recreate with current properties/bindings and replay all completed loads
    /// in order. Module caches are fresh; the host loader is called again.
    /// Discard unfinished loads, runtime state and debugger handles. Replace this
    /// VM only after replay succeeds; external host side effects cannot roll back.
    pub fn reset(&mut self) -> Result<(), VmError> {
        // 実ゲームではVMのclose/newstateだけではComposite出力キャッシュは
        // zero化されません（E20）。再生成をまたいで出力値を保持します。
        let saved_output = self.io.load_output();
        let config = MicrocontrollerConfig {
            properties: read_state(&self.state).properties.clone(),
            limits: self.limits,
            environment: self.environment,
            bindings: self.bindings.clone(),
            control_namespace: self.control_namespace.clone(),
            require_loader: self.require_loader.clone(),
            random: self.random.clone(),
            deterministic_random: self.deterministic_random,
        };
        let mut next = Self::new(config)?;
        if self.dev_logs {
            next.enable_dev_logs()?;
        }
        for chunk in &self.sources {
            next.load(&chunk.source, &chunk.name)?;
        }
        next.io.store_output(&saved_output);
        // 再生中のトップレベルをpollで中断させないよう、再生が終わってから引き継ぎます。
        #[cfg(feature = "debug")]
        next.set_pause_poll(self.pause_poll.clone())?;
        *self = next;
        Ok(())
    }
}

/// Shared read access to vehicle state. Leaf lock, never held across a Lua call.
pub(crate) fn read_state(state: &RwLock<State>) -> RwLockReadGuard<'_, State> {
    state.read().unwrap_or_else(PoisonError::into_inner)
}

/// Exclusive write access to vehicle state. Leaf lock, never held across a Lua call.
pub(crate) fn write_state(state: &RwLock<State>) -> RwLockWriteGuard<'_, State> {
    state.write().unwrap_or_else(PoisonError::into_inner)
}

/// Borrowed view of the retained draw commands; holds a shared read lock for its lifetime.
pub struct Commands<'a>(RwLockReadGuard<'a, State>);
impl Deref for Commands<'_> {
    type Target = [DrawCommand];
    fn deref(&self) -> &[DrawCommand] {
        self.0.commands.commands()
    }
}

/// Borrowed view of the host properties; holds a shared read lock for its lifetime.
pub struct Properties<'a>(RwLockReadGuard<'a, State>);
impl Deref for Properties<'_> {
    type Target = PropertyBag;
    fn deref(&self) -> &PropertyBag {
        &self.0.properties
    }
}

fn http_error(error: storm_lua_spec::http::HttpError) -> VmError {
    VmError::new(
        if error == storm_lua_spec::http::HttpError::Limit {
            ErrorKind::Limit
        } else {
            ErrorKind::InvalidArgument
        },
        error.to_string(),
    )
}
impl Microcontroller {
    /// ホスト管理トランスポート向けに新規リクエストを取り出します（ネットワーク通信は送信しません）。
    pub fn drain_http_requests(&mut self) -> Vec<storm_lua_spec::http::HttpRequest> {
        write_state(&self.state).http.drain()
    }
    /// 静止状態（pending / outbox が空）でのみ割当カーソルを返します。
    ///
    /// 飛行中の transport は再開できないため、呼び出し側が先に排出してください。
    /// その契約は [`storm_lua_spec::http::HttpQueue::continuation_cursor`] が所有し、
    /// ここでは転送のみを行います。
    pub fn http_continuation_cursor(&self) -> Result<u32, VmError> {
        self.vm.ensure_idle()?;
        let state = read_state(&self.state);
        state.http.continuation_cursor().map_err(|_| {
            VmError::new(
                ErrorKind::Unsupported,
                "http continuation requires drained requests",
            )
        })
    }
    /// 静止キューを新世代で復元します。旧トークンは無効化されます。
    ///
    /// 呼び出し側は排出済みの状態でカーソルを取得している必要があります。
    /// この操作はカーソルだけを運び、飛行中のリクエストを再開しません。
    pub fn restore_http_continuation(&mut self, cursor: u32) -> Result<(), VmError> {
        self.vm.ensure_idle()?;
        let queue = storm_lua_spec::http::HttpQueue::from_continuation_cursor(cursor)
            .map_err(http_error)?;
        write_state(&self.state).http = queue;
        Ok(())
    }
    /// アイドル境界で1件のレスポンスを配信します。ビジー状態による拒否時にはトークンを消費しません。
    pub fn http_reply(
        &mut self,
        token: storm_lua_spec::http::HttpToken,
        reply: &[u8],
    ) -> Result<RunOutcome, VmError> {
        self.vm.ensure_idle()?;
        let request = write_state(&self.state)
            .http
            .reply(token, reply)
            .map_err(http_error)?;
        use storm_lua_vm::value::LuaValue;
        // httpReplyはエラー状態でも配送されます（実ゲームはtick末尾の保守処理を継続します、E9）。
        self.io.entry_error_status.store(5, Ordering::Relaxed);
        let result = self.vm.call_with(
            "httpReply",
            &[
                LuaValue::Integer(i64::from(request.port)),
                LuaValue::Bytes(request.request),
                LuaValue::Bytes(reply.to_vec()),
            ],
        );
        self.finish(result)
    }
    /// 送信済みリクエストをキャンセルします。ホストは実際のトランスポート失敗を別途報告します。
    pub fn cancel_http(&mut self, token: storm_lua_spec::http::HttpToken) -> Result<(), VmError> {
        write_state(&self.state)
            .http
            .cancel(token)
            .map_err(http_error)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use storm_lua_spec::property::PropertyValue;

    #[test]
    fn fast_io_and_shared_read_guards_coexist_without_deadlock() {
        let mut props = PropertyBag::default();
        props.insert(b"gain".to_vec(), PropertyValue::Number(2.0));
        let mut mc = Microcontroller::new(MicrocontrollerConfig {
            properties: props,
            ..Default::default()
        })
        .unwrap();
        mc.load(
            br#"
            local g = property.getNumber("gain")
            function onTick()
                output.setNumber(1, input.getNumber(1) * g)
                output.setBool(32, input.getBool(32))
            end
            function onDraw()
                screen.setColor(10, 20, 30)
                screen.drawRectF(0, 0, screen.getWidth(), screen.getHeight())
                screen.drawText(1, 1, 42)
            end
        "#,
            "=fast-io",
        )
        .unwrap();
        let mut input = CompositeSignal::default();
        input.numbers[0] = 3.5;
        input.booleans[31] = true;
        mc.tick(&input).unwrap();
        mc.draw(64, 32).unwrap();

        let cmds = mc.commands();
        let props_view = mc.properties();
        assert_eq!(cmds.len(), 3);
        assert_eq!(mc.output().numbers[0], 7.0);
        assert!(mc.output().booleans[31]);
        assert_eq!(mc.input().numbers[0], 3.5);
        assert_eq!(props_view.len(), 1);
    }

    #[test]
    fn replay_releases_state_lock_before_invoking_sink_and_fast_io_survives_snapshot() {
        let mut mc = Microcontroller::new(Default::default()).unwrap();
        mc.load(
            br#"
            local acc = 0
            function onTick()
                acc = acc + input.getNumber(1)
                output.setNumber(1, acc)
            end
            function onDraw()
                screen.setColor(255, 128, 64)
                screen.drawClear()
            end
        "#,
            "=replay-snap",
        )
        .unwrap();
        let mut input = CompositeSignal::default();
        input.numbers[0] = 5.0;
        mc.tick(&input).unwrap();
        mc.draw(32, 32).unwrap();

        struct ReentrantSink<'a> {
            state: &'a Arc<RwLock<State>>,
            count: usize,
        }
        impl ScreenSink for ReentrantSink<'_> {
            fn submit(
                &mut self,
                _command: &DrawCommand,
            ) -> Result<(), storm_lua_spec::draw::ScreenError> {
                let guard = write_state(self.state);
                assert_eq!(guard.commands.commands().len(), 2);
                drop(guard);
                self.count += 1;
                Ok(())
            }
        }
        let mut sink = ReentrantSink {
            state: &mc.state,
            count: 0,
        };
        mc.replay(&mut sink).unwrap();
        assert_eq!(sink.count, 2);

        let snap = mc.snapshot().unwrap();
        let saved_out = mc.output();
        let mut restored = Microcontroller::new(Default::default()).unwrap();
        restored.restore(&snap).unwrap();
        restored.set_output(saved_out).unwrap();
        assert_eq!(restored.output().numbers[0], 5.0);
        input.numbers[0] = 7.0;
        restored.tick(&input).unwrap();
        assert_eq!(restored.output().numbers[0], 12.0);
    }

    #[test]
    fn screen_stack_arg_preserves_v050_explicit_nil_arity_and_phase_rules() {
        let mut mc = Microcontroller::new(Default::default()).unwrap();
        mc.load(
            br#"
            function onDraw()
                screen.setColor(255, 0, 0)
                screen.setColor(255, 0, 0, nil)
                screen.drawLine(1, 2)
                screen.drawText(1, 2)
                screen.drawTextBox(1, 2, 3, 4)
                screen.drawTextBox(1, 2, 3, 4, 'hi')
                screen.drawTextBox(1, 2, 3, 4, nil, nil, 0.5)
            end
            function onTick()
                screen.setColor(255, 0, 0)
            end
        "#,
            "=screen-v050",
        )
        .unwrap();
        mc.draw(32, 32).unwrap();
        let cmds = mc.commands();
        assert_eq!(cmds.len(), 4);
        assert_eq!(
            cmds[0],
            DrawCommand::SetColor(storm_lua_spec::screen::Rgba8([255, 0, 0, 255]))
        );
        assert_eq!(
            cmds[1],
            DrawCommand::SetColor(storm_lua_spec::screen::Rgba8([255, 0, 0, 0]))
        );
        assert_eq!(
            cmds[2],
            DrawCommand::TextBox([1.0, 2.0, 3.0, 4.0, -1.0, -1.0], b"hi".to_vec())
        );
        assert_eq!(
            cmds[3],
            DrawCommand::TextBox([1.0, 2.0, 3.0, 4.0, 0.0, 0.5], Vec::new())
        );
        drop(cmds);

        let err = mc.tick(&CompositeSignal::default()).unwrap_err();
        assert_eq!(err.kind, ErrorKind::Lua);
        assert!(err
            .message
            .contains("attempting to use screen function outside onDraw"));
        assert_eq!(mc.error_status(), 2);
    }

    #[test]
    fn bug_t2_07_replay_drops_state_lock_under_pending_writer_contention() {
        use std::time::Duration;
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut mc = Microcontroller::new(MicrocontrollerConfig::default()).unwrap();
            mc.load(
                b"function onDraw() screen.setColor(255, 0, 0) screen.drawClear() end",
                "=draw-contended",
            )
            .unwrap();
            assert_eq!(mc.draw(32, 32).unwrap(), RunOutcome::Completed);

            struct ContendedSink<'a> {
                mc: &'a Microcontroller,
                state: Arc<RwLock<State>>,
                count: usize,
            }
            impl ScreenSink for ContendedSink<'_> {
                fn submit(
                    &mut self,
                    _command: &DrawCommand,
                ) -> Result<(), storm_lua_spec::draw::ScreenError> {
                    let state_for_writer = Arc::clone(&self.state);
                    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
                    let writer = std::thread::spawn(move || {
                        ready_tx.send(()).unwrap();
                        write_state(&state_for_writer)
                            .properties
                            .insert(b"k".to_vec(), PropertyValue::Number(99.0));
                    });
                    ready_rx.recv().unwrap();
                    std::thread::sleep(Duration::from_millis(10));
                    assert!(self.mc.properties().len() <= 1);
                    writer.join().unwrap();
                    self.count += 1;
                    Ok(())
                }
            }

            let state_clone = Arc::clone(&mc.state);
            let mut sink = ContendedSink {
                mc: &mc,
                state: state_clone,
                count: 0,
            };
            mc.replay(&mut sink).unwrap();
            assert_eq!(sink.count, 2);
            assert_eq!(
                mc.properties().get(b"k".as_slice()),
                Some(&PropertyValue::Number(99.0))
            );
            let _ = tx.send(());
        });

        rx.recv_timeout(Duration::from_millis(1500))
            .expect("BUG-T2-07: Microcontroller::replay deadlocked under pending writer");
    }
}
