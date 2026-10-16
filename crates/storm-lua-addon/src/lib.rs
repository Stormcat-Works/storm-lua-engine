//! アドオンLuaは独立したプロファイルであり、追加のグローバル変数を持つマイクロコントローラーではありません。
//! ワールド状態および同期的なserver関数はホスト側から提供されます。
mod bindings;
mod callbacks;
pub use callbacks::{AddonCallback, ScopedServerCall, ScopedServerFunction};
#[cfg(feature = "debug")]
mod debugger;
mod matrix;
#[cfg(test)]
mod tests;
use std::{
    cell::{Cell, RefCell},
    collections::BTreeMap,
    rc::Rc,
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
            scoped_server: Vec::new(),
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
    vm: Rc<RefCell<Vm>>,
    callbacks: AddonCallback,
    config: AddonConfig,
    state: Rc<RefCell<State>>,
    stage: Rc<Cell<Stage>>,
    source: Vec<u8>,
    source_name: String,
    tick_disabled: Rc<Cell<bool>>,
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
        }
