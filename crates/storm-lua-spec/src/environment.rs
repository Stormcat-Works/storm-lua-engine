//! Script-visible environment contracts, shared by runtime and compiler.
use serde::{Deserialize, Serialize};

/// Script-visible functionality, independent from Vehicle/Addon and host debugging.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EnvironmentProfile {
    /// Game-facing API surface. Development helpers are not injected.
    #[default]
    Game,
    /// Explicit embedded/development extensions; not an actual-game compatibility claim.
    Extended,
}

/// Standard global functions installed in the game-facing environment.
/// 実ゲームのbase部分集合は6関数のみで、`select` は両クラスとも不在です（E13）。
pub const GAME_FUNCTIONS: &[&str] = &["ipairs", "next", "pairs", "tonumber", "tostring", "type"];
/// Additional Lua globals exposed only by the extended environment.
pub const EXTENDED_FUNCTIONS: &[&str] = &[
    "assert", "error", "pcall", "xpcall", "unpack", "print", "select",
];
/// Globals absent from the game-facing environment. `debug` exists with `log` only.
/// `require` is handled separately by the static project linker.
pub const GAME_UNAVAILABLE: &[&str] = &[
    "print",
    "pcall",
    "xpcall",
    "error",
    "assert",
    "select",
    "setmetatable",
    "getmetatable",
    "rawget",
    "rawset",
    "rawequal",
    "rawlen",
    "load",
    "loadstring",
    "loadfile",
    "dofile",
    "unpack",
    "os",
    "io",
    "coroutine",
    "collectgarbage",
    "package",
    "_G",
];
/// Builtin roots visible to a Vehicle script, including the restricted logging table.
pub const VEHICLE_ROOTS: &[&str] = &[
    "math", "input", "output", "property", "screen", "string", "table", "type", "ipairs", "pairs",
    "next", "tonumber", "tostring", "map", "self", "debug", "async",
];
impl EnvironmentProfile {
    /// Whether this selected profile leaves a known Lua builtin absent.
    pub fn is_unavailable(self, name: &str) -> bool {
        GAME_UNAVAILABLE.contains(&name)
            && !(self == Self::Extended && EXTENDED_FUNCTIONS.contains(&name))
    }
    /// Addon-specific base-function availability. `select` は実ゲームでは
    /// Vehicle/Addonの両クラスとも不在のため、現在は `is_unavailable` と同一です。
    pub fn is_addon_unavailable(self, name: &str) -> bool {
        self.is_unavailable(name)
    }
    /// Component Mod base-function availability. select is absent from the
    /// shared game environment itself (E13), so this currently matches
    /// the base predicate.
    pub fn is_component_unavailable(self, name: &str) -> bool {
        self.is_unavailable(name)
    }
    /// Stable wire/profile identifier.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Game => "game",
            Self::Extended => "extended",
        }
    }
}

/// Validate a dot-separated host binding path. Environment and lifecycle ownership stay with the SDK.
pub fn valid_binding_path(path: &str) -> bool {
    !path.is_empty()
        && path.len() <= 128
        && path.split('.').all(|part| {
            part != "_ENV"
                && part != "_G"
                && !part.is_empty()
                && part.bytes().enumerate().all(|(i, b)| {
                    b == b'_' || b.is_ascii_alphabetic() || (i > 0 && b.is_ascii_digit())
                })
        })
}

/// Builtin roots visible to a Component Mod script, including the restricted logging table.
pub const COMPONENT_ROOTS: &[&str] = &[
    "math", "string", "table", "debug", "matrix", "parser", "component", "ipairs", "next", "pairs",
    "tonumber", "tostring", "type",
];
/// Vehicle callbacks invoked externally, not ordinary dead global functions.
pub const VEHICLE_CALLBACKS: &[&str] = &["onTick", "onDraw", "httpReply"];
/// Component Mod callbacks invoked externally, not ordinary dead global functions.
pub const COMPONENT_CALLBACKS: &[&str] = &[
    "onParse",
    "onAddToSimulation",
    "onTick",
    "onRender",
    "onRemoveFromSimulation",
];
