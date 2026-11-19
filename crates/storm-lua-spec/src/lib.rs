//! バックエンド非依存のドメイン契約。Lua、グラフィックス、ネットワーク、プラットフォーム依存を持ちません。
pub mod abi;
pub mod io;
pub mod property;
pub mod screen;

pub mod draw;

pub mod command_wire;

/// 探索可能なエンジンAPI契約。
pub mod catalog;

/// ホスト管理のHTTPトランスポート契約。
pub mod http;

/// アドオン固有のAPIおよびコールバックカタログ。
pub mod addon;

/// Component Mod固有のAPIおよびコールバックカタログ。
pub mod component;

/// ホストマップ描画およびパレット契約。
pub mod map;

/// Script-visible game and explicitly extended environments.
pub mod environment;
