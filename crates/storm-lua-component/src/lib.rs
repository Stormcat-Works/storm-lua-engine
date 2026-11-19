//! Stormworks の Component Mod（コンポーネント埋め込み Lua）のプロファイルです。
//!
//! Component Mod は車両/アドオンとは別の第 3 のスクリプトクラスで、
//! コンポーネント定義の `lua_filename` 属性が指すスクリプトを、コンポーネント
//! 1 個につき 1 インスタンスで実行します。ライフサイクルは
//! `load → parse → add_to_simulation → tick / render → remove_from_simulation`
//! の順序付き API で駆動し、名前空間は `component` / `parser` / `matrix` と
//! base 環境の subset です。スロットや描画などのホスト依存機能は
//! [`ComponentProviders`] で提供されたドメインの分だけ登録されます。
mod bindings;
#[cfg(feature = "debug")]
mod debugger;
/// ホスト提供ドメインのトレイトとプロバイダ集合。
pub mod host;
#[cfg(test)]
#[allow(clippy::expect_used, clippy::panic, clippy::type_complexity)]
mod tests;

use host::ComponentProviders;
use storm_lua_vm::{
    bindings::HostBindings,
    logging::LogRecord,
    random::SharedCrtRandom,
    runner::{ErrorKind, RunOutcome, Vm, VmError},
    value::LuaValue,
    ExecutionLimits,
};

/// Component インスタンスの生成時入力。
#[derive(Clone, Default)]
pub struct ComponentConfig {
    /// すべてのエントリポイントで強制される実行制限（命令数・時間・メモリ等）。
    pub limits: ExecutionLimits,
    /// Script-visible game or explicitly extended environment.
    pub environment: storm_lua_spec::environment::EnvironmentProfile,
    /// Explicit host extensions.
    pub bindings: HostBindings,
    /// ホストが供給する Windows CRT 実行コンテキストの共有ストリーム。
    /// None の場合はエンジン内蔵の共有 CRT ストリーム（ゲーム既定、J2）。
    pub random: Option<SharedCrtRandom>,
    /// true の場合、Game プロファイルの既定（共有 CRT 互換ストリーム）ではなく
    /// VM ごとの決定的ストリームを使います（J2 のホスト選択肢）。
    /// `random` を指定した場合はそちらが優先します。
    pub deterministic_random: bool,
    /// ホストが提供するドメイン実装。提供された分だけ Lua へ登録されます。
    pub providers: ComponentProviders,
}

/// 実ゲームのスクリプトエラー状態（ステータス）の写し。
/// 0=正常、2=onTick 失敗、3=ロード失敗、5=onTick 以外のコールバック失敗。
/// Component の timeout は「0 以外」までしか確定していないため、数値を主張しない
/// variant として表します（U6）。Addon と違い自動復帰はなく、失敗した
/// インスタンスはホストが作り直します（U5）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ComponentStatus {
    /// 正常（0）。
    #[default]
    Ok,
    /// onTick の Lua エラー（2）。
    TickFailed,
    /// ロード時の Lua エラー（3）。
    LoadFailed,
    /// onTick 以外のコールバックの Lua エラー（5）。
    CallbackFailed,
    /// 実行時間制限による停止。数値コードは未確定です。
    TimedOut,
}

impl ComponentStatus {
    /// 確定している数値コード。timeout は None です。
    pub fn code(self) -> Option<u32> {
        match self {
            Self::Ok => Some(0),
            Self::TickFailed => Some(2),
            Self::LoadFailed => Some(3),
            Self::CallbackFailed => Some(5),
            Self::TimedOut => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stage {
    Empty,
    Loading,
    Loaded,
    Parsing,
    Parsed,
    Adding,
    InSimulation,
    Ticking,
    Rendering,
    Removing,
    Removed,
    Failed,
}

fn invalid(message: &str) -> VmError {
    VmError::new(ErrorKind::InvalidArgument, message)
}

/// VM の時間制限エラーは ErrorKind::Lua の通常エラーとして届くため、
/// 文面の接頭辞で timeout と判別します（構造化マーカーは VM 側にありません）。
fn is_timeout_error(error: &VmError) -> bool {
    error.message.starts_with("max execution time exceeded")
}

/// 1 コンポーネント分の Component Mod スクリプトインスタンス。
pub struct Component {
    vm: Vm,
    status: ComponentStatus,
    stage: Stage,
    entry_status: ComponentStatus,
}

impl Component {
    /// プロバイダと環境を検証し、VM と名前空間を構築します。
    pub fn new(config: ComponentConfig) -> Result<Self, VmError> {
        config.bindings.validate(config.environment)?;
        let mut vm = Vm::with_component_environment(config.limits, config.environment)?;
        if let Some(random) = &config.random {
            vm.install_crt_random(std::sync::Arc::clone(random))?;
        } else if config.deterministic_random {
            vm.use_deterministic_random()?;
        }
        let providers = config.providers.clone();
        vm.configure(|lua, env| bindings::install(lua, env, &providers))?;
        vm.install_bindings(&config.bindings)?;
        // GAP-T2-02: インストールしたホスト名前空間をスナップショットの
        // シンボル解決基盤へ登録する（debug.log ラッパ・component.*・
        // parser.*・matrix.*）。
        let host_paths = vm.configure(|_, env| bindings::host_paths(env))?;
        for path in &host_paths {
            let segments: Vec<&str> = path.iter().map(String::as_str).collect();
            vm.register_host_path(&segments)?;
        }
        Ok(Self {
            vm,
            status: ComponentStatus::Ok,
            stage: Stage::Empty,
            entry_status: ComponentStatus::Ok,
        })
    }

    /// 現在のエラーステータス。
    pub fn status(&self) -> ComponentStatus {
        self.status
    }

    /// VM が回復不能な失敗状態かどうか。
    pub fn is_failed(&self) -> bool {
        self.vm.is_failed()
    }

    /// ブレークポイント等で中断中かどうか。
    pub fn is_suspended(&self) -> bool {
        self.vm.is_suspended()
    }

    /// 開発用ログ（`debug.log` の記録）を有効化します。
    pub fn enable_dev_logs(&mut self) -> Result<(), VmError> {
        self.vm.enable_logs()
    }

    /// 蓄積したログ記録を取り出します。
    pub fn drain_log_records(&mut self) -> Vec<LogRecord> {
        self.vm.drain_log_records()
    }

    fn require(&self, stage: Stage) -> Result<(), VmError> {
        self.vm.ensure_idle()?;
        if self.stage == Stage::Failed {
            return Err(VmError::new(
                ErrorKind::Failed,
                "recreate the component after failure",
            ));
        }
        if self.stage != stage {
            return Err(invalid("component lifecycle operation is out of order"));
        }
        Ok(())
    }

    /// スクリプトのトップレベルコードを 1 度実行します。
    ///
    /// `name` は定義 XML の `lua_filename` 属性値をそのまま渡すのがホスト契約です
    /// （docs/specs/component.md）。Lua エラーでもライフサイクルは Loaded へ進み、
    /// ステータスに 3 が記録されます。
    pub fn load(&mut self, source: &[u8], name: &str) -> Result<RunOutcome, VmError> {
        self.require(Stage::Empty)?;
        if name.len() > 1024 {
            return Err(invalid("component source name is too large"));
        }
        self.stage = Stage::Loading;
        self.entry_status = ComponentStatus::LoadFailed;
        let result = self.vm.execute(source, name);
        self.finish(result)
    }

    /// `onParse` を 1 度呼び出します（定義データの読み書き期間）。
    /// コールバックが未定義でも遷移は完了し、ステータスは変わりません。
    pub fn parse(&mut self) -> Result<RunOutcome, VmError> {
        self.require(Stage::Loaded)?;
        self.stage = Stage::Parsing;
        self.entry_status = ComponentStatus::CallbackFailed;
        let result = self.vm.call_with("onParse", &[]);
        self.finish(result)
    }

    /// `onAddToSimulation` を 1 度呼び出します。
    pub fn add_to_simulation(&mut self) -> Result<RunOutcome, VmError> {
        self.require(Stage::Parsed)?;
        self.stage = Stage::Adding;
        self.entry_status = ComponentStatus::CallbackFailed;
        let result = self.vm.call_with("onAddToSimulation", &[]);
        self.finish(result)
    }

    /// `onTick(game_ticks)` を呼びます。`game_ticks` は実ゲームどおり整数で
    /// 渡され、0 も拒否されません。
    ///
    /// tick だけがステータスゲートを持ちます: ステータスが正常でない間は
    /// コールバックを実行せず `RunOutcome::Missing` を返します（自動復帰なし）。
    pub fn tick(&mut self, game_ticks: u32) -> Result<RunOutcome, VmError> {
        self.require(Stage::InSimulation)?;
        if self.status != ComponentStatus::Ok {
            return Ok(RunOutcome::Missing);
        }
        self.stage = Stage::Ticking;
        self.entry_status = ComponentStatus::TickFailed;
        let result = self
            .vm
            .call_with("onTick", &[LuaValue::Integer(i64::from(game_ticks))]);
        self.finish(result)
    }

    /// `onRender` を呼びます。ステータスゲートはありません。
    pub fn render(&mut self) -> Result<RunOutcome, VmError> {
        self.require(Stage::InSimulation)?;
        self.stage = Stage::Rendering;
        self.entry_status = ComponentStatus::CallbackFailed;
        let result = self.vm.call_with("onRender", &[]);
        self.finish(result)
    }

    /// `onRemoveFromSimulation` を呼びます。tick 失敗後でも配送されます
    /// （ステータスゲートはありません）。
    pub fn remove_from_simulation(&mut self) -> Result<RunOutcome, VmError> {
        self.require(Stage::InSimulation)?;
        self.stage = Stage::Removing;
        self.entry_status = ComponentStatus::CallbackFailed;
        let result = self.vm.call_with("onRemoveFromSimulation", &[]);
        self.finish(result)
    }

    fn finish(&mut self, result: Result<RunOutcome, VmError>) -> Result<RunOutcome, VmError> {
        if matches!(result, Ok(RunOutcome::Suspended)) {
            return result;
        }
        if let Err(error) = &result {
            if error.kind != ErrorKind::Lua || self.vm.is_failed() {
                self.stage = Stage::Failed;
                return result;
            }
            // 通常のLuaエラーは実ゲームのエラーステータスへ記録します。
            // ステータスは自動復帰せず、tick は正常に戻るまで抑止されます。
            self.status =
                if self.entry_status == ComponentStatus::TickFailed && is_timeout_error(error) {
                    ComponentStatus::TimedOut
                } else {
                    self.entry_status
                };
        }
        self.stage = match self.stage {
            Stage::Loading => Stage::Loaded,
            Stage::Parsing => Stage::Parsed,
            Stage::Adding | Stage::Ticking | Stage::Rendering => Stage::InSimulation,
            Stage::Removing => Stage::Removed,
            other => other,
        };
        result
    }
}
