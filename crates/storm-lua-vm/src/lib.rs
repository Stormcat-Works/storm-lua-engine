//! バジェット管理されたLua 5.3の実行、サンドボックス、およびオプションのホストデバッガを管理します。
//! backend-mlua フィーチャーは、バックエンド密結合な明示的拡張／テスト用のエスケープハッチです。
#[cfg(feature = "debug")]
pub mod debug;

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

/// 有効なLua浮動小数点数のビット幅（すべてのビルドターゲットで検証されます）。
pub const LUA_NUMBER_BITS: usize = std::mem::size_of::<mlua::Number>() * 8;
/// 有効なLua整数のビット幅（ポインタ幅とは独立）。
pub const LUA_INTEGER_BITS: usize = std::mem::size_of::<mlua::Integer>() * 8;
const _: () = assert!(LUA_NUMBER_BITS == 64 && LUA_INTEGER_BITS == 64);

/// ホスト実行制限。タイマー、スレッド、非同期ランタイムはここでは管理しません。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExecutionLimits {
    /// コールバックごとの決定論的な命令数バジェット。デバッガによる中断をまたいで保持されます。
    pub instruction_budget: std::num::NonZeroU64,
    /// Luaメモリ確保の上限。ホスト管理バッファは独自の上限を設ける必要があります。
    pub lua_memory_bytes: std::num::NonZeroUsize,
}

mod backend_support;
pub mod runner;
mod sandbox;

impl Default for ExecutionLimits {
    fn default() -> Self {
        Self {
            instruction_budget: std::num::NonZeroU64::MIN.saturating_add(999_999),
            lua_memory_bytes: std::num::NonZeroUsize::MIN.saturating_add(8 * 1024 * 1024 - 1),
        }
    }
}

/// VM単位の構造化ログ。
pub mod logging;
/// 損失のないホスト／イベント／セーブデータ値。
pub mod value;

/// Trusted host value/function bindings, separate from script-visible standard libraries.
pub mod bindings;
/// Synchronous borrowed host arguments and protected predicates.
pub mod host;
/// Owned source chunks and explicit development-time include loading.
pub mod source;

/// Explicit host-shared native CRT random context, separate from isolated defaults.
pub mod random;
