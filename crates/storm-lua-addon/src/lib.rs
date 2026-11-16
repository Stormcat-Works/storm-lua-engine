//! アドオンLuaは独立したプロファイルであり、追加のグローバル変数を持つマイクロコントローラーではありません。
//! ワールド状態および同期的なserver関数はホスト側から提供されます。
mod bindings;
mod callbacks;
pub use callbacks::{AddonCallback, ScopedServerCall, ScopedServerFunction};
#[cfg(feature = "debug")]
mod debugger;
pub use storm_lua_vm::matrix;
#[cfg(test)]
mod tests;
use std::{
    cell::{Cell, RefCell},
    collections::BTreeMap,
    rc::{Rc, Weak},
};
use storm_lua_spec::{
    http::{HttpError, HttpQueue, HttpRequest, HttpToken},
    property::PropertyBag,
};
use storm_lua_vm::{
    logging::LogRecord,
    runner::{ErrorKind, RunOutcome, Vm, VmError},
    value::{HostFunction, LuaValue},
    ExecutionLimits,
};

/// 生成時入力。保存されたテーブルはトップレベル実行の後、start() の前に復元されます。
#[derive(Clone)]
pub struct AddonConfig {
    /// すべてのエントリポイントで強制される、Luaの命令数およびヒープメモリの上限。
    pub limits: ExecutionLimits,
    /// 新規作成されたワールドの場合のみtrue。メニュープロパティの戻り値を制御します。
    pub is_world_create: bool,
    /// ホストによって選択された新規ワールドのプロパティ値。
    pub properties: PropertyBag,
    /// 既存ワールドに対する直近のホストチェックポイント。非循環なデータテーブルである必要があります。
    pub savedata: Option<LuaValue>,
    /// 修飾なしの名前でインデックス付けされた、明示的に実装された同期的なserver関数群。
    pub server: BTreeMap<String, HostFunction>,
    /// Server operations supplied for each borrowed host execution phase.
    /// Including `httpGet` transfers request enqueue ownership to the host.
    pub scoped_server: Vec<String>,
    /// printを追加します。制限付きのdebug.logはアドオンプロファイルで既定で利用可能です。
    pub dev_logs: bool,
    /// Script-visible game or explicitly extended environment.
    pub environment: storm_lua_spec::environment::EnvironmentProfile,
    /// Explicit host extensions, retained across savedata reload.
    pub bindings: storm_lua_vm::bindings::HostBindings,
    /// Optional Windows CRT execution-context stream shared with the host scene.
    /// None selects the engine's built-in shared CRT stream (the game default, J2);
    /// the host saves state only for a context it supplies itself.
    pub random: Option<storm_lua_vm::random::SharedCrtRandom>,
    /// trueの場合、Gameプロファイルの既定（共有CRT互換ストリーム）ではなく
    /// VMごとの決定的ストリームを使います（J2のホスト選択肢）。
    /// `random` を指定した場合はそちらが優先します。
    pub deterministic_random: bool,
    /// Explicit development require; cache is recreated during savedata reload.
    pub require_loader: Option<storm_lua_vm::source::RequireLoader>,
}
impl Default for AddonConfig {
    fn default() -> Self {
        Self {
            limits: ExecutionLimits::default(),
            is_world_create: true,
            properties: PropertyBag::default(),
            savedata: None,
            server: BTreeMap::new(),
            scoped_server: Vec::new(),
            dev_logs: false,
            environment: Default::default(),
            bindings: Default::default(),
            require_loader: None,
            random: None,
            deterministic_random: false,
        }
    }
}
/// アドオンが宣言したメニュープロパティ。UI描画はホストの責務です。
#[derive(Debug, Clone, PartialEq)]
pub enum MenuProperty {
    /// チェックボックスの既定値。
    Checkbox(bool),
    /// 最小値、最大値、増分、および既定値。
    Slider([f64; 4]),
}
struct State {
    new_world: bool,
    properties: PropertyBag,
    definitions: BTreeMap<Vec<u8>, MenuProperty>,
    http: HttpQueue,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stage {
    Empty,
    Loading,
    Loaded,
    Creating,
    Ready,
    Callback,
    Destroying,
    Destroyed,
    Failed,
}
/// 実ゲームのエラーステータス（スクリプト状態の +0x78 相当）とエラーリング。
/// 0=正常、2=onTick失敗、3=ロード失敗、5=onTick以外のコールバック失敗。
/// ステータス5は報告後に自動復帰し、ステータス2/3はリロードまで残ります（E9）。
#[derive(Default)]
pub(crate) struct ErrorState {
    status: Cell<u32>,
    errors: RefCell<Vec<String>>,
}
impl ErrorState {
    pub(crate) fn status(&self) -> u32 {
        self.status.get()
    }
    /// 通常のLuaエラーを記録します。後からの失敗がステータスを上書きします。
    pub(crate) fn record(&self, status: u32, message: &str) {
        self.status.set(status);
        let mut errors = self.errors.borrow_mut();
        if errors.len() > 10 {
            errors.remove(0);
        }
        errors.push(message.to_owned());
    }
    /// ステータス5の報告完了に伴うクリア（ステータス・リング・未報告分）です。
    pub(crate) fn clear(&self) {
        self.status.set(0);
        self.errors.borrow_mut().clear();
    }
    pub(crate) fn log(&self) -> Vec<String> {
        self.errors.borrow().clone()
    }
}

/// A synchronous entry for host-generated Addon events, including reentrant events.
/// Handles share the actual VM and lifecycle; reload/drop invalidates older handles.
#[derive(Clone)]
pub struct SynchronousEvents {
    callbacks: storm_lua_vm::runner::SynchronousCallbacks,
    stage: Weak<Cell<Stage>>,
    error_state: Weak<ErrorState>,
    scoped: bool,
}
impl SynchronousEvents {
    /// Whether this script still admits events. Failed or destroyed scripts are excluded.
    pub fn is_live(&self) -> bool {
        self.stage
            .upgrade()
            .is_some_and(|s| !matches!(s.get(), Stage::Failed | Stage::Destroyed))
            && self.callbacks.is_live()
    }
    /// Execute a documented event before its triggering host call returns.
    /// Errors retain side effects. Ordinary Lua errors are recorded as status 5
    /// (auto-recovering; other events remain deliverable); resource failures stop
    /// immediately. Host state is not rolled back.
    pub fn dispatch(&self, callback: &str, arguments: &[LuaValue]) -> Result<RunOutcome, VmError> {
        if !storm_lua_spec::addon::EVENTS.contains(&callback) {
            return Err(invalid("unknown or reserved addon callback"));
        }
        let stage = self
            .stage
            .upgrade()
            .ok_or_else(|| invalid("addon event owner was dropped"))?;
        if matches!(stage.get(), Stage::Failed | Stage::Destroyed) {
            return Err(VmError::new(
                ErrorKind::Failed,
                "addon no longer admits callbacks",
            ));
        }
        self.callbacks.ensure_available()?;
        let result = self.callbacks.call_if_function(callback, arguments);
        if let Err(error) = &result {
            if error.kind == ErrorKind::Lua && self.callbacks.is_live() {
                if let Some(error_state) = self.error_state.upgrade() {
                    error_state.record(5, &error.message);
                }
            } else if !self.scoped || !self.callbacks.is_live() {
                stage.set(Stage::Failed);
            }
        }
        result
    }
}

/// One host call's event scope. Errors are reported immediately; host callback
/// failures become terminal at the end of the pass. Lua errors only stop ticks.
pub struct SynchronousEventPass {
    pass: Option<storm_lua_vm::runner::SynchronousCallbackPass>,
    events: SynchronousEvents,
    owner: SynchronousEvents,
}
impl SynchronousEventPass {
    /// A cloneable event entry whose lifetime is bounded by this guard.
    pub fn events(&self) -> SynchronousEvents {
        self.events.clone()
    }
}
impl Drop for SynchronousEventPass {
    fn drop(&mut self) {
        self.pass.take();
        if !self.owner.callbacks.is_live() {
            if let Some(stage) = self.owner.stage.upgrade() {
                if stage.get() != Stage::Destroyed {
                    stage.set(Stage::Failed);
                }
            }
        }
    }
}

/// 1つのアドオンLua状態。Composite I/O、画面API、ワールドシミュレータ、スケジューラは含みません。
pub struct Addon {
    vm: Rc<RefCell<Vm>>,
    callbacks: AddonCallback,
    config: AddonConfig,
    state: Rc<RefCell<State>>,
    stage: Rc<Cell<Stage>>,
    source: Vec<u8>,
    source_name: String,
    error_state: Rc<ErrorState>,
    /// 実行中の入口に対応するエラーステータス。継続の再開でも入口の値を保ちます。
    entry_error_status: Cell<u32>,
}
fn invalid(message: &str) -> VmError {
    VmError::new(ErrorKind::InvalidArgument, message)
}
fn http_error(error: HttpError) -> VmError {
    VmError::new(
        if error == HttpError::Limit {
            ErrorKind::Limit
        } else {
            ErrorKind::InvalidArgument
        },
        error.to_string(),
    )
}
impl Addon {
    /// Obtain an event entry for synchronous world services without borrowing an executing Addon.
    pub fn synchronous_events(&self) -> SynchronousEvents {
        SynchronousEvents {
            callbacks: self.vm.borrow().synchronous_callbacks(),
            stage: Rc::downgrade(&self.stage),
            error_state: Rc::downgrade(&self.error_state),
            scoped: false,
        }
    }
    /// Admit nested callbacks for one host root call, retaining ordinary errors
    /// until its scope ends. Resource failures stop immediately.
    pub fn begin_synchronous_event_pass(&self) -> Result<SynchronousEventPass, VmError> {
        let owner = self.synchronous_events();
        let pass = owner.callbacks.begin_pass()?;
        let events = SynchronousEvents {
            callbacks: pass.callbacks(),
            stage: owner.stage.clone(),
            error_state: owner.error_state.clone(),
            scoped: true,
        };
        Ok(SynchronousEventPass {
            pass: Some(pass),
            events,
            owner,
        })
    }
    /// アドオン固有のAPI名前空間のみを持つ未開始のアドオンを作成します。
    pub fn new(config: AddonConfig) -> Result<Self, VmError> {
        if config.is_world_create && config.savedata.is_some() {
            return Err(invalid("a new world cannot restore existing savedata"));
        }
        if config.server.len() + config.scoped_server.len() > 512 {
            return Err(invalid("too many server bindings"));
        }
        for name in config.server.keys().chain(config.scoped_server.iter()) {
            // A scoped httpGet explicitly transfers enqueue ownership to the
            // scene host. The built-in bounded queue remains the default.
            if (name == "httpGet" && !config.scoped_server.iter().any(|n| n == "httpGet"))
                || name.len() > 128
                || name.is_empty()
                || !name.bytes().enumerate().all(|(i, b)| {
                    b == b'_' || b.is_ascii_alphabetic() || (i > 0 && b.is_ascii_digit())
                })
            {
                return Err(invalid("invalid or reserved server function name"));
            }
        }
        let mut names = std::collections::BTreeSet::new();
        if config
            .server
            .keys()
            .chain(config.scoped_server.iter())
            .any(|name| !names.insert(name))
        {
            return Err(invalid("duplicate server function name"));
        }
        config.bindings.validate(config.environment)?;
        if let Some(loader) = &config.require_loader {
            loader.validate_configuration(config.environment, &config.bindings)?;
        }
        for path in config
            .bindings
            .values
            .keys()
            .chain(config.bindings.functions.keys())
        {
            if path == "server"
                || config
                    .server
                    .keys()
                    .chain(config.scoped_server.iter())
                    .any(|name| {
                        path == &format!("server.{name}")
                            || path.starts_with(&format!("server.{name}."))
                    })
            {
                return Err(invalid(
                    "host bindings overlap the configured server namespace",
                ));
            }
        }
        let mut vm = Vm::with_addon_environment(config.limits, config.environment)?;
        if let Some(random) = &config.random {
            vm.install_crt_random(Rc::clone(random))?;
        } else if config.deterministic_random {
            vm.use_deterministic_random()?;
        }
        if let Some(savedata) = &config.savedata {
            if !matches!(savedata, LuaValue::Table(_)) {
                return Err(invalid("savedata must be a table"));
            }
            // ソースコードや外部サービスが実行される前にチェックポイントを検証します。
            vm.set_global("g_savedata", savedata)?;
        }
        vm.set_global("g_savedata", &LuaValue::Table(Vec::new()))?;
        let state = Rc::new(RefCell::new(State {
            new_world: config.is_world_create,
            properties: config.properties.clone(),
            definitions: BTreeMap::new(),
            http: HttpQueue::new().map_err(http_error)?,
        }));
        // select は環境定義そのものに含まれないため、ここでの個別の除去は不要です（E13）。
        vm.configure(|lua, env| bindings::install(lua, env, Rc::clone(&state)))?;
        vm.install_bindings(&config.bindings)?;
        if let Some(loader) = &config.require_loader {
            vm.install_require_loader(loader)?;
        }
        if config.dev_logs {
            vm.enable_logs()?;
        }
        for (name, handler) in &config.server {
            vm.register_function("server", name, Rc::clone(handler))?;
        }
        let stage = Rc::new(Cell::new(Stage::Empty));
        let error_state = Rc::new(ErrorState::default());
        let vm = Rc::new(RefCell::new(vm));
        let callbacks = AddonCallback::new(
            Rc::clone(&vm),
            Rc::clone(&stage),
            Rc::clone(&error_state),
            &config.scoped_server,
        )?;
        Ok(Self {
            vm,
            callbacks,
            config,
            state,
            stage,
            source: Vec::new(),
            source_name: String::new(),
            error_state,
            entry_error_status: Cell::new(3),
        })
    }
    fn require(&self, stage: Stage) -> Result<(), VmError> {
        self.vm.borrow_mut().ensure_idle()?;
        if self.stage.get() == Stage::Failed {
            return Err(VmError::new(
                ErrorKind::Failed,
                "reload the addon after failure",
            ));
        }
        if self.stage.get() != stage {
            return Err(invalid("addon lifecycle operation is out of order"));
        }
        Ok(())
    }
    /// トップレベルコードを1度実行します。Luaエラー時も既存のsavedataを復元し、startを許可します。
    pub fn load(&mut self, source: &[u8], name: &str) -> Result<RunOutcome, VmError> {
        self.require(Stage::Empty)?;
        // ソース長制限はホストが有効化した場合だけ適用します（実ゲームにハード制限はありません）。
        if name.len() > 1024
            || self
                .config
                .limits
                .max_source_bytes
                .is_some_and(|max| source.len() > max.get())
        {
            return Err(invalid("addon source or name is too large"));
        }
        self.source = source.to_vec();
        self.source_name = name.to_owned();
        self.stage.set(Stage::Loading);
        self.entry_error_status.set(3);
        let _execution = self.callbacks.enter();
        let result = self.vm.borrow_mut().execute(source, name);
        self.finish(result)
    }
    /// loadの後にonCreateを1度呼び出します。コールバックが存在しない場合でもライフサイクル遷移は完了します。
    pub fn start(&mut self) -> Result<RunOutcome, VmError> {
        self.require(Stage::Loaded)?;
        self.stage.set(Stage::Creating);
        let _execution = self.callbacks.enter();
        self.entry_error_status.set(5);
        let result = self
            .vm
            .borrow_mut()
            .call_with("onCreate", &[LuaValue::Bool(self.config.is_world_create)]);
        self.finish(result)
    }
    /// Composite信号ではなくゲーム進行倍率を用いて、アドオンの1 tickを駆動します。
    /// game_ticksは実ゲームでは符号付き整数として渡され、0以下も拒否されません（E18）。
    /// エラー状態の間はonTickをスキップします。ステータス5は報告済みのため
    /// この呼び出しでクリアされ、onTickは次のtickから再開します（E9）。
    pub fn tick(&mut self, game_ticks: i32) -> Result<RunOutcome, VmError> {
        self.require(Stage::Ready)?;
        match self.error_state.status() {
            0 => {}
            5 => {
                self.error_state.clear();
                return Ok(RunOutcome::Missing);
            }
            _ => return Ok(RunOutcome::Missing),
        }
        self.run_event("onTick", &[LuaValue::Integer(i64::from(game_ticks))], 2)
    }
    /// ドキュメント化されたアドオンコールバックをディスパッチします。ライフサイクル／tick／HTTPコールバックは専用メソッドを使用します。
    pub fn dispatch(
        &mut self,
        callback: &str,
        arguments: &[LuaValue],
    ) -> Result<RunOutcome, VmError> {
        self.require(Stage::Ready)?;
        self.callbacks.dispatch(callback, arguments)
    }
    /// Obtain a callback target independent of the mutable host borrow.
    pub fn callback_target(&self) -> AddonCallback {
        self.callbacks.clone()
    }
    fn run_event(
        &mut self,
        callback: &str,
        arguments: &[LuaValue],
        error_status: u32,
    ) -> Result<RunOutcome, VmError> {
        self.stage.set(Stage::Callback);
        self.entry_error_status.set(error_status);
        let _execution = self.callbacks.enter();
        let result = self.vm.borrow_mut().call_with(callback, arguments);
        self.finish(result)
    }
    /// onDestroyを明示的に呼び出します。破棄処理自体がユーザーコードを暗黙に実行することはありません。
    pub fn destroy(&mut self) -> Result<RunOutcome, VmError> {
        self.require(Stage::Ready)?;
        self.stage.set(Stage::Destroying);
        self.entry_error_status.set(5);
        let _execution = self.callbacks.enter();
        let result = self.vm.borrow_mut().call("onDestroy");
        self.finish(result)
    }
    fn finish(&mut self, result: Result<RunOutcome, VmError>) -> Result<RunOutcome, VmError> {
        if matches!(result, Ok(RunOutcome::Suspended)) {
            return result;
        }
        if let Err(error) = &result {
            if error.kind != ErrorKind::Lua || self.vm.borrow_mut().is_failed() {
                self.stage.set(Stage::Failed);
                return result;
            }
            // 通常のLuaエラーは実ゲームのエラーステータスとリングへ記録します（E9）。
            // onTickの恒久抑止はしません（ステータス5は次のtickで自動復帰します）。
            self.error_state
                .record(self.entry_error_status.get(), &error.message);
        }
        self.stage.set(match self.stage.get() {
            Stage::Loading => {
                if let Some(savedata) = &self.config.savedata {
                    if let Err(error) = self.vm.borrow_mut().set_global("g_savedata", savedata) {
                        self.stage.set(Stage::Failed);
                        return Err(error);
                    }
                }
                Stage::Loaded
            }
            Stage::Creating | Stage::Callback => Stage::Ready,
            Stage::Destroying => Stage::Destroyed,
            other => other,
        });
        result
    }
    /// Luaを実行せずに現在のg_savedataをコピーします（VM全体のスナップショットやゲームXMLではありません）。
    pub fn savedata(&self) -> Result<LuaValue, VmError> {
        if matches!(self.stage.get(), Stage::Empty | Stage::Loading) {
            return Err(invalid("savedata is not initialized"));
        }
        let value = self.vm.borrow_mut().global("g_savedata")?;
        if !matches!(value, LuaValue::Table(_)) {
            return Err(invalid("g_savedata is not a table"));
        }
        Ok(value)
    }
    /// 明示的なホストチェックポイントを用いて再作成およびロードを行います。その後 start()（onCreate(false)）を呼び出してください。
    /// 置換成功後は古いHTTP／デバッグ識別子は無効となり、外部への影響はロールバックできません。
    pub fn reload(&mut self, savedata: LuaValue) -> Result<RunOutcome, VmError> {
        self.reload_with(savedata, Self::load)
    }
    /// Reload using a host loading phase created for the replacement Addon.
    /// The loader can obtain the new callback target before installing borrowed
    /// operations; handlers must not retain the old target. Replacement occurs
    /// only after the loader succeeds. Call start() separately afterward.
    pub fn reload_with(
        &mut self,
        savedata: LuaValue,
        load: impl FnOnce(&mut Addon, &[u8], &str) -> Result<RunOutcome, VmError>,
    ) -> Result<RunOutcome, VmError> {
        if self.stage.get() == Stage::Empty {
            return Err(invalid("no addon source to reload"));
        }
        let mut config = self.config.clone();
        config.is_world_create = false;
        config.savedata = Some(savedata);
        let mut next = Self::new(config)?;
        let outcome = load(&mut next, &self.source, &self.source_name)?;
        *self = next;
        Ok(outcome)
    }
    /// property.checkbox/sliderによって行われた宣言（トップレベル実行時を含む）のスナップショット。
    pub fn property_definitions(&self) -> BTreeMap<Vec<u8>, MenuProperty> {
        self.state.borrow().definitions.clone()
    }
    /// 明示的なprint拡張を有効化します。debug.logは制限付きのログ専用テーブルのまま維持されます。
    pub fn enable_dev_logs(&mut self) -> Result<(), VmError> {
        self.vm.borrow_mut().enable_logs()?;
        self.config.dev_logs = true;
        Ok(())
    }
    /// 完了、中断、またはエラーの前後にログを取り出します。
    pub fn drain_log_records(&mut self) -> Vec<LogRecord> {
        self.vm.borrow_mut().drain_log_records()
    }
    /// ホストトランスポート向けに送信HTTPリクエストを取り出します。
    pub fn drain_http_requests(&mut self) -> Vec<HttpRequest> {
        self.state.borrow_mut().http.drain()
    }
    /// コールバックの合間にレスポンスを配信します。ビジー状態または不正ステージによる拒否時には未処理トークンを保持します。
    pub fn http_reply(&mut self, token: HttpToken, reply: &[u8]) -> Result<RunOutcome, VmError> {
        self.require(Stage::Ready)?;
        let request = self.resolve_http_reply(token, reply)?;
        self.dispatch_http_reply(u32::from(request.port), &request.request, reply)
    }
    /// hostが完了させたHTTP応答を、このscriptへ配送します。
    /// tokenの消費とcontainer内の配送順はhostが所有します。
    pub fn dispatch_http_reply(
        &mut self,
        port: u32,
        request: &[u8],
        reply: &[u8],
    ) -> Result<RunOutcome, VmError> {
        self.require(Stage::Ready)?;
        self.run_event(
            "httpReply",
            &[
                LuaValue::Integer(i64::from(port)),
                LuaValue::Bytes(request.to_vec()),
                LuaValue::Bytes(reply.to_vec()),
            ],
            5,
        )
    }
    /// 応答を検証し、送信済みtokenを消費して元要求を返します。
    /// 複数scriptへの配送を所有するhost用で、callbackは実行しません。
    /// VMのdestroy/failure後もsettlementでき、Luaは実行しません。
    /// 拒否時にはtokenを保持します。
    pub fn resolve_http_reply(
        &mut self,
        token: HttpToken,
        reply: &[u8],
    ) -> Result<HttpRequest, VmError> {
        self.state
            .borrow_mut()
            .http
            .reply(token, reply)
            .map_err(http_error)
    }
    /// 成功レスポンスを捏造することなく、実際のトランスポート失敗後にキャンセルします。
    pub fn cancel_http(&mut self, token: HttpToken) -> Result<(), VmError> {
        self.state
            .borrow_mut()
            .http
            .cancel(token)
            .map_err(http_error)
    }
    /// アドオンコールバックがデバッグのために停止（中断）しているかどうか。
    pub fn is_suspended(&self) -> bool {
        self.vm.borrow_mut().is_suspended()
    }
    /// 制限超過またはホスト境界の失敗により再作成が必要かどうか。
    /// 通常のLuaエラーではfalseのまま通常イベントを配送できます。
    pub fn is_failed(&self) -> bool {
        self.stage.get() == Stage::Failed || self.vm.borrow_mut().is_failed()
    }
    /// 実ゲームのエラーステータス（0=正常、2=onTick失敗、3=ロード失敗、
    /// 5=onTick以外のコールバック失敗）。ステータス5は次のtickで自動復帰します。
    pub fn error_status(&self) -> u32 {
        self.error_state.status()
    }
    /// 記録されたエラーメッセージ（最大11件、古い順）。
    pub fn error_log(&self) -> Vec<String> {
        self.error_state.log()
    }
}

impl Drop for Addon {
    fn drop(&mut self) {
        self.callbacks.invalidate();
    }
}
