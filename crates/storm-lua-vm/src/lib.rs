//! バジェット管理されたLua 5.3の実行、サンドボックス、およびオプションのホストデバッガを管理します。
//! backend-mlua フィーチャーは、mlua バックエンドそのものへの依存ゲートであり、
//! バックエンド密結合な明示的拡張／テスト用のエスケープハッチでもあります。
//! 無効時は実行バックエンドを持たず、方言変換とロード合法性判定
//! （load_legality、判定は自前パーサによる代替）だけを提供します。
#[cfg(feature = "debug")]
pub mod debug;
pub mod dialect;
pub mod load_legality;

/// バックエンド密結合アクセス。通常のエンジンAPIではなく、mluaへの互換性保証でもありません。
#[cfg(feature = "backend-mlua")]
pub mod backend {
    /// Encode owned host values with the VM's normal size and depth limits.
    pub fn encode_values(
        lua: &Lua,
        values: &[crate::value::LuaValue],
    ) -> Result<MultiValue, crate::runner::VmError> {
        crate::value::encode(lua, values)
    }
    /// Decode callback arguments with the VM's normal size and depth limits.
    pub fn decode_values(
        values: MultiValue,
    ) -> Result<Vec<crate::value::LuaValue>, crate::runner::VmError> {
        crate::value::decode(values)
    }
    /// 独立した統合テストおよび数値境界テストに必要な直接のバックエンド型。
    pub use mlua::{
        Error as BackendError, Function, Lua, MultiValue, Result as BackendResult,
        String as LuaString, Table, Value, Variadic,
    };
}

/// 有効なLua浮動小数点数のビット幅（Lua 5.3 の既定の数値型は64bit浮動小数点）。
pub const LUA_NUMBER_BITS: usize = 64;
/// 有効なLua整数のビット幅（ポインタ幅とは独立。Lua 5.3 の既定の整数型は64bit）。
pub const LUA_INTEGER_BITS: usize = 64;
#[cfg(feature = "backend-mlua")]
const _: () = assert!(
    std::mem::size_of::<mlua::Number>() * 8 == LUA_NUMBER_BITS
        && std::mem::size_of::<mlua::Integer>() * 8 == LUA_INTEGER_BITS
);

/// ホスト実行制限。タイマー、スレッド、非同期ランタイムはここでは管理しません。
///
/// 実ゲームの制限モデルに合わせ、既定で有効なのは壁時計の時間制限だけです
/// （[`ExecutionLimits::time_limit`]、実ゲームと同じ1001ms）。命令数・メモリ・
/// ソース長は実ゲームにハード制限が存在しないため既定で無効（`None`）であり、
/// ホストが明示的に有効化した場合だけ適用されます。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExecutionLimits {
    /// コールバックごとの決定論的な命令数バジェット。デバッガによる中断をまたいで保持されます。
    /// `None`（既定）では無制限です。
    pub instruction_budget: Option<std::num::NonZeroU64>,
    /// Luaメモリ確保の上限。ホスト管理バッファは独自の上限を設ける必要があります。
    /// `None`（既定）では無制限です。
    pub lua_memory_bytes: Option<std::num::NonZeroUsize>,
    /// 1回の入口実行あたりの壁時計制限。既定は実ゲームと同じ1001msで、
    /// 超過時は実ゲームと同じ文面のLuaエラーになります。`None`で無効化できます。
    pub time_limit: Option<std::time::Duration>,
    /// 1チャンクのソース長上限（バイト）。`None`（既定）では無制限です。
    /// 有効化すると、保持プログラム全体の上限（チャンク数・合計バイト数）も併せて適用されます。
    pub max_source_bytes: Option<std::num::NonZeroUsize>,
}

impl ExecutionLimits {
    /// 実ゲームの時間制限（1001ms）。超過メッセージの文面は実ゲームの表記どおり1000msです。
    pub const GAME_TIME_LIMIT: std::time::Duration = std::time::Duration::from_millis(1001);
    /// 命令数バジェットの実効値。無効時は枯渇しない値です。
    pub(crate) fn instruction_allowance(&self) -> u64 {
        self.instruction_budget
            .map_or(u64::MAX, |budget| budget.get())
    }
    /// 時間制限超過エラーの接頭辞。実ゲームは `max execution time exceeded (1000ms)` です。
    pub(crate) fn timeout_prefix(&self) -> String {
        let limit = self.time_limit.unwrap_or(Self::GAME_TIME_LIMIT);
        let millis = if limit == Self::GAME_TIME_LIMIT {
            1000
        } else {
            limit.as_millis()
        };
        format!("max execution time exceeded ({millis}ms)")
    }
    /// 時間制限超過エラーの文面。実ゲームは `max execution time exceeded (1000ms). stopped at line <N>` です。
    pub(crate) fn timeout_message(&self, line: Option<i32>) -> String {
        format!(
            "{}. stopped at line {}",
            self.timeout_prefix(),
            line.filter(|line| *line > 0).unwrap_or(0)
        )
    }
}

#[cfg(feature = "backend-mlua")]
mod backend_support;
#[cfg(feature = "backend-mlua")]
pub mod runner;
#[cfg(feature = "backend-mlua")]
mod sandbox;

impl Default for ExecutionLimits {
    fn default() -> Self {
        Self {
            instruction_budget: None,
            lua_memory_bytes: None,
            time_limit: Some(Self::GAME_TIME_LIMIT),
            max_source_bytes: None,
        }
    }
}

/// VM単位の構造化ログ。
#[cfg(feature = "backend-mlua")]
pub mod logging;
/// 損失のないホスト／イベント／セーブデータ値。
#[cfg(feature = "backend-mlua")]
pub mod value;

/// Trusted host value/function bindings, separate from script-visible standard libraries.
#[cfg(feature = "backend-mlua")]
pub mod bindings;
/// Synchronous borrowed host arguments and protected predicates.
#[cfg(feature = "backend-mlua")]
pub mod host;
/// Owned source chunks and explicit development-time include loading.
#[cfg(feature = "backend-mlua")]
pub mod source;

/// Explicit host-shared native CRT random context, separate from isolated defaults.
#[cfg(feature = "backend-mlua")]
pub mod random;
