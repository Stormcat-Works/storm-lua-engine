//! アドオンLuaは独立したプロファイルであり、追加のグローバル変数を持つマイクロコントローラーではありません。
//! ワールド状態および同期的なserver関数はホスト側から提供されます。
mod bindings;
#[cfg(feature = "debug")]
mod debugger;
mod matrix;
use std::{cell::RefCell, collections::BTreeMap, rc::Rc};
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
    /// printを追加します。制限付きのdebug.logはアドオンプロファイルで既定で利用可能です。
    pub dev_logs: bool,
    /// Script-visible game or explicitly extended environment.
    pub environment: storm_lua_spec::environment::EnvironmentProfile,
    /// Explicit host extensions, retained across savedata reload.
    pub bindings: storm_lua_vm::bindings::HostBindings,
    /// Optional Windows CRT execution-context stream shared with the host scene.
    /// None retains the isolated deterministic default. The host saves its state.
    pub random: Option<storm_lua_vm::random::SharedCrtRandom>,
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
            dev_logs: false,
            environment: Default::default(),
            bindings: Default::default(),
            require_loader: None,
            random: None,
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
/// 1つのアドオンLua状態。Composite I/O、画面API、ワールドシミュレータ、スケジューラは含みません。
pub struct Addon {
    vm: Vm,
    config: AddonConfig,
    state: Rc<RefCell<State>>,
    stage: Stage,
    source: Vec<u8>,
    source_name: String,
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
    /// アドオン固有のAPI名前空間のみを持つ未開始のアドオンを作成します。
    pub fn new(config: AddonConfig) -> Result<Self, VmError> {
        if config.is_world_create && config.savedata.is_some() {
            return Err(invalid("a new world cannot restore existing savedata"));
        }
        if config.server.len() > 512 {
            return Err(invalid("too many server bindings"));
        }
        for name in config.server.keys() {
            if name == "httpGet"
                || name.len() > 128
                || name.is_empty()
                || !name.bytes().enumerate().all(|(i, b)| {
                    b == b'_' || b.is_ascii_alphabetic() || (i > 0 && b.is_ascii_digit())
                })
            {
                return Err(invalid("invalid or reserved server function name"));
            }
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
                || config.server.keys().any(|name| {
                    path == &format!("server.{name}")
                        || path.starts_with(&format!("server.{name}."))
                })
            {
                return Err(invalid(
                    "host bindings overlap the configured server namespace",
                ));
            }
        }
        let mut vm = Vm::with_environment(config.limits, config.environment)?;
        if let Some(random) = &config.random {
            vm.install_crt_random(Rc::clone(random))?;
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
        Ok(Self {
            vm,
            config,
            state,
            stage: Stage::Empty,
            source: Vec::new(),
            source_name: String::new(),
        })
    }
    fn require(&self, stage: Stage) -> Result<(), VmError> {
        self.vm.ensure_idle()?;
        if self.stage == Stage::Failed {
            return Err(VmError::new(
                ErrorKind::Failed,
                "reload the addon after failure",
            ));
        }
        if self.stage != stage {
            return Err(invalid("addon lifecycle operation is out of order"));
        }
        Ok(())
    }
    /// トップレベルコードを1度実行します。既存のsavedataはこれが完了した後にのみ適用されます（再開後も同様）。
    pub fn load(&mut self, source: &[u8], name: &str) -> Result<RunOutcome, VmError> {
        self.require(Stage::Empty)?;
        if source.len() > 1024 * 1024 || name.len() > 1024 {
            return Err(invalid("addon source or name is too large"));
        }
        self.source = source.to_vec();
        self.source_name = name.to_owned();
        self.stage = Stage::Loading;
        let result = self.vm.execute(source, name);
        self.finish(result)
    }
    /// loadの後にonCreateを1度呼び出します。コールバックが存在しない場合でもライフサイクル遷移は完了します。
    pub fn start(&mut self) -> Result<RunOutcome, VmError> {
        self.require(Stage::Loaded)?;
        self.stage = Stage::Creating;
        let result = self
            .vm
            .call_with("onCreate", &[LuaValue::Bool(self.config.is_world_create)]);
        self.finish(result)
    }
    /// Composite信号ではなく経過ゲームtick数を用いて、アドオンの1 tickを駆動します。
    pub fn tick(&mut self, game_ticks: u32) -> Result<RunOutcome, VmError> {
        self.require(Stage::Ready)?;
        if game_ticks == 0 {
            return Err(invalid("game_ticks must be positive"));
        }
        self.run_event("onTick", &[LuaValue::Integer(i64::from(game_ticks))])
    }
    /// ドキュメント化されたアドオンコールバックをディスパッチします。ライフサイクル／tick／HTTPコールバックは専用メソッドを使用します。
    pub fn dispatch(
        &mut self,
        callback: &str,
        arguments: &[LuaValue],
    ) -> Result<RunOutcome, VmError> {
        self.require(Stage::Ready)?;
        if !storm_lua_spec::addon::EVENTS.contains(&callback) {
            return Err(invalid("unknown or reserved addon callback"));
        }
        self.run_event(callback, arguments)
    }
    fn run_event(&mut self, callback: &str, arguments: &[LuaValue]) -> Result<RunOutcome, VmError> {
        self.stage = Stage::Callback;
        let result = self.vm.call_with(callback, arguments);
        self.finish(result)
    }
    /// onDestroyを明示的に呼び出します。破棄処理自体がユーザーコードを暗黙に実行することはありません。
    pub fn destroy(&mut self) -> Result<RunOutcome, VmError> {
        self.require(Stage::Ready)?;
        self.stage = Stage::Destroying;
        let result = self.vm.call("onDestroy");
        self.finish(result)
    }
    fn finish(&mut self, result: Result<RunOutcome, VmError>) -> Result<RunOutcome, VmError> {
        match &result {
            Ok(RunOutcome::Suspended) => {}
            Err(_) => self.stage = Stage::Failed,
            Ok(_) => {
                self.stage = match self.stage {
                    Stage::Loading => {
                        if let Some(savedata) = &self.config.savedata {
                            if let Err(error) = self.vm.set_global("g_savedata", savedata) {
                                self.stage = Stage::Failed;
                                return Err(error);
                            }
                        }
                        Stage::Loaded
                    }
                    Stage::Creating | Stage::Callback => Stage::Ready,
                    Stage::Destroying => Stage::Destroyed,
                    other => other,
                };
            }
        }
        result
    }
    /// Luaを実行せずに現在のg_savedataをコピーします（VM全体のスナップショットやゲームXMLではありません）。
    pub fn savedata(&self) -> Result<LuaValue, VmError> {
        if matches!(self.stage, Stage::Empty | Stage::Loading) {
            return Err(invalid("savedata is not initialized"));
        }
        let value = self.vm.global("g_savedata")?;
        if !matches!(value, LuaValue::Table(_)) {
            return Err(invalid("g_savedata is not a table"));
        }
        Ok(value)
    }
    /// 明示的なホストチェックポイントを用いて再作成およびロードを行います。その後 start()（onCreate(false)）を呼び出してください。
    /// 置換成功後は古いHTTP／デバッグ識別子は無効となり、外部への影響はロールバックできません。
    pub fn reload(&mut self, savedata: LuaValue) -> Result<RunOutcome, VmError> {
        if self.stage == Stage::Empty {
            return Err(invalid("no addon source to reload"));
        }
        let mut config = self.config.clone();
        config.is_world_create = false;
        config.savedata = Some(savedata);
        let mut next = Self::new(config)?;
        let outcome = next.load(&self.source, &self.source_name)?;
        *self = next;
        Ok(outcome)
    }
    /// property.checkbox/sliderによって行われた宣言（トップレベル実行時を含む）のスナップショット。
    pub fn property_definitions(&self) -> BTreeMap<Vec<u8>, MenuProperty> {
        self.state.borrow().definitions.clone()
    }
    /// 明示的なprint拡張を有効化します。debug.logは制限付きのログ専用テーブルのまま維持されます。
    pub fn enable_dev_logs(&mut self) -> Result<(), VmError> {
        self.vm.enable_logs()?;
        self.config.dev_logs = true;
        Ok(())
    }
    /// 完了、中断、またはエラーの前後にログを取り出します。
    pub fn drain_log_records(&mut self) -> Vec<LogRecord> {
        self.vm.drain_log_records()
    }
    /// ホストトランスポート向けに送信HTTPリクエストを取り出します。
    pub fn drain_http_requests(&mut self) -> Vec<HttpRequest> {
        self.state.borrow_mut().http.drain()
    }
    /// コールバックの合間にレスポンスを配信します。ビジー状態または不正ステージによる拒否時には未処理トークンを保持します。
    pub fn http_reply(&mut self, token: HttpToken, reply: &[u8]) -> Result<RunOutcome, VmError> {
        self.require(Stage::Ready)?;
        let request = self
            .state
            .borrow_mut()
            .http
            .reply(token, reply)
            .map_err(http_error)?;
        self.run_event(
            "httpReply",
            &[
                LuaValue::Integer(i64::from(request.port)),
                LuaValue::Bytes(request.request),
                LuaValue::Bytes(reply.to_vec()),
            ],
        )
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
        self.vm.is_suspended()
    }
    /// 実行時エラーまたはライフサイクル初期化失敗により再作成が必要かどうか。
    pub fn is_failed(&self) -> bool {
        self.stage == Stage::Failed || self.vm.is_failed()
    }
}
