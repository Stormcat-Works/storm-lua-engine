//! CPUラスタライズから独立したマイクロコントローラーLua APIおよびコールバックのライフサイクル。
mod bindings;
mod controls;
#[cfg(feature = "debug")]
mod debugger;
use std::{
    cell::{Ref, RefCell},
    rc::Rc,
};
use storm_lua_spec::{
    draw::{CommandBuffer, DrawCommand, ScreenSink},
    io::CompositeSignal,
    property::PropertyBag,
};
/// VM所有者と共有されるコールバック完了状態。
pub use storm_lua_vm::runner::RunOutcome as CallbackOutcome;
use storm_lua_vm::{
    runner::{ErrorKind, RunOutcome, Vm, VmError},
    ExecutionLimits,
};

/// トップレベルコードが実行される前にプロパティが組み込まれます。
#[derive(Debug, Clone, Default)]
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
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    Idle,
    Init,
    Tick,
    Draw,
}
struct State {
    input: CompositeSignal,
    input_changed: bool,
    output: CompositeSignal,
    properties: PropertyBag,
    phase: Phase,
    size: (u32, u32),
    http: storm_lua_spec::http::HttpQueue,
    commands: CommandBuffer,
}
/// 明示的なtick/draw駆動と再利用可能なコマンドストレージを備えたスレッド拘束型VM。
pub struct Microcontroller {
    vm: Vm,
    state: Rc<RefCell<State>>,
    limits: ExecutionLimits,
    sources: Vec<storm_lua_vm::source::SourceChunk>,
    source_bytes: usize,
    pending_source: Option<storm_lua_vm::source::SourceChunk>,
    require_loader: Option<storm_lua_vm::source::RequireLoader>,
    dev_logs: bool,
    environment: storm_lua_spec::environment::EnvironmentProfile,
    bindings: storm_lua_vm::bindings::HostBindings,
    control_namespace: Option<String>,
}
impl Microcontroller {
    /// 未開始のコントローラーを作成します。loadを呼び出す前にホストオプションを設定してください。
    pub fn new(config: MicrocontrollerConfig) -> Result<Self, VmError> {
        config.bindings.validate(config.environment)?;
        if let Some(loader) = &config.require_loader {
            loader.validate_configuration(config.environment, &config.bindings)?;
        }
        let mut vm = Vm::with_environment(config.limits, config.environment)?;
        let state = Rc::new(RefCell::new(State {
            input: CompositeSignal::default(),
            input_changed: false,
            output: CompositeSignal::default(),
            properties: config.properties,
            phase: Phase::Idle,
            size: (0, 0),
            http: storm_lua_spec::http::HttpQueue::new().map_err(http_error)?,
            commands: CommandBuffer::new(65536, 1024 * 1024),
        }));
        vm.configure(|lua, env| bindings::install(lua, env, Rc::clone(&state)))?;
        let bindings = controls::with_controls(
            &config.bindings,
            config.control_namespace.as_deref(),
            config.environment,
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
        if let Some(loader) = &config.require_loader {
            vm.install_require_loader(loader)?;
        }
        Ok(Self {
            vm,
            state,
            limits: config.limits,
            sources: Vec::new(),
            source_bytes: 0,
            pending_source: None,
            require_loader: config.require_loader,
            dev_logs: false,
            environment: config.environment,
            bindings: config.bindings,
            control_namespace: config.control_namespace,
        })
    }
    /// Execute another named chunk in the existing environment. Completed loads
    /// are retained in order for reset; this appends rather than replacing code.
    /// Chunk locals are distinct. A failed or abandoned suspended load is not saved.
    pub fn load(&mut self, source: &[u8], name: &str) -> Result<RunOutcome, VmError> {
        self.vm.ensure_idle()?;
        storm_lua_vm::source::validate_source(source, name)?;
        if self.sources.len() >= storm_lua_vm::source::MAX_PROGRAM_CHUNKS
            || source.len() > storm_lua_vm::source::MAX_PROGRAM_BYTES - self.source_bytes
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
        self.state.borrow_mut().phase = Phase::Init;
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
        {
            let mut state = self.state.borrow_mut();
            state.input = *input;
            state.phase = Phase::Tick;
        }
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
        {
            let mut state = self.state.borrow_mut();
            state.phase = Phase::Draw;
            state.size = (width, height);
            state.commands.clear();
        }
        let result = self.vm.call_with(name, arguments);
        self.finish(result)
    }
    fn finish(&mut self, result: Result<RunOutcome, VmError>) -> Result<RunOutcome, VmError> {
        if !matches!(result, Ok(RunOutcome::Suspended)) {
            self.state.borrow_mut().phase = Phase::Idle;
            if let Some(chunk) = self.pending_source.take() {
                if result.is_ok() {
                    self.source_bytes += chunk.source.len();
                    self.sources.push(chunk);
                }
            }
        }
        result
    }
    /// Snapshot the current input, including changes made by explicit development controls.
    pub fn input(&self) -> CompositeSignal {
        self.state.borrow().input
    }
    /// Drain an input update made by development controls. None leaves pending host input intact.
    pub fn take_input_changes(&mut self) -> Option<CompositeSignal> {
        let mut state = self.state.borrow_mut();
        if !state.input_changed {
            return None;
        }
        state.input_changed = false;
        Some(state.input)
    }
    /// ネイティブ信号の小さなスナップショットをコピーします（JSON変換は不要）。
    pub fn output(&self) -> CompositeSignal {
        self.state.borrow().output
    }
    /// 中断中の一部のプレフィックスを含む、順序付けられた描画コマンドを参照します。
    pub fn commands(&self) -> Ref<'_, [DrawCommand]> {
        Ref::map(self.state.borrow(), |state| state.commands.commands())
    }
    /// ラスタライズ依存を導入することなく、保持されている描画コマンドを再生します。
    pub fn replay(
        &self,
        sink: &mut dyn ScreenSink,
    ) -> Result<(), storm_lua_spec::draw::ScreenError> {
        self.state.borrow().commands.replay(sink)
    }
    /// アイドル境界で実行時プロパティをすべて置換します。既存のLuaローカル変数には影響しません。
    pub fn set_properties(&mut self, properties: PropertyBag) -> Result<(), VmError> {
        self.vm.ensure_idle()?;
        if self.control_namespace.is_some() {
            controls::validate_properties(&properties)?;
        }
        self.state.borrow_mut().properties = properties;
        Ok(())
    }
    /// テキストデコードや数値の型縮小を行わずにホストプロパティデータを検査します。
    pub fn properties(&self) -> Ref<'_, PropertyBag> {
        Ref::map(self.state.borrow(), |state| &state.properties)
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
    /// 実行時エラーの発生後にこのVMをリセットする必要があるかどうか。
    pub fn is_failed(&self) -> bool {
        self.vm.is_failed()
    }
    /// Recreate with current properties/bindings and replay all completed loads
    /// in order. Module caches are fresh; the host loader is called again.
    /// Discard unfinished loads, runtime state and debugger handles. Replace this
    /// VM only after replay succeeds; external host side effects cannot roll back.
    pub fn reset(&mut self) -> Result<(), VmError> {
        let config = MicrocontrollerConfig {
            properties: self.state.borrow().properties.clone(),
            limits: self.limits,
            environment: self.environment,
            bindings: self.bindings.clone(),
            control_namespace: self.control_namespace.clone(),
            require_loader: self.require_loader.clone(),
        };
        let mut next = Self::new(config)?;
        if self.dev_logs {
            next.enable_dev_logs()?;
        }
        for chunk in &self.sources {
            next.load(&chunk.source, &chunk.name)?;
        }
        *self = next;
        Ok(())
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
        self.state.borrow_mut().http.drain()
    }
    /// アイドル境界で1件のレスポンスを配信します。ビジー状態による拒否時にはトークンを消費しません。
    pub fn http_reply(
        &mut self,
        token: storm_lua_spec::http::HttpToken,
        reply: &[u8],
    ) -> Result<RunOutcome, VmError> {
        self.vm.ensure_idle()?;
        let request = self
            .state
            .borrow_mut()
            .http
            .reply(token, reply)
            .map_err(http_error)?;
        use storm_lua_vm::value::LuaValue;
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
        self.state
            .borrow_mut()
            .http
            .cancel(token)
            .map_err(http_error)
    }
}
