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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExecutionLimits {
    /// コールバックごとの決定論的な命令数バジェット。デバッガによる中断をまたいで保持されます。
    pub instruction_budget: std::num::NonZeroU64,
    /// Luaメモリ確保の上限。ホスト管理バッファは独自の上限を設ける必要があります。
    pub lua_memory_bytes: std::num::NonZeroUsize,
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
            instruction_budget: std::num::NonZeroU64::MIN.saturating_add(999_999),
            lua_memory_bytes: std::num::NonZeroUsize::MIN.saturating_add(8 * 1024 * 1024 - 1),
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
