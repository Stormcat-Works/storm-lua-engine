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
pub const GAME_FUNCTIONS: &[&str] = &[
    "ipairs", "next", "pairs", "select", "tonumber", "tostring", "type",
];
/// Additional Lua globals exposed only by the extended environment.
pub const EXTENDED_FUNCTIONS: &[&str] = &["assert", "error", "pcall", "xpcall", "unpack", "print"];
/// Globals absent from the game-facing environment. `debug` exists with `log` only.
/// `require` is handled separately by the static project linker.
pub const GAME_UNAVAILABLE: &[&str] = &[
    "print",
    "pcall",
    "xpcall",
    "error",
    "assert",
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
    "next", "select", "tonumber", "tostring", "map", "self", "debug", "async",
];
impl EnvironmentProfile {
    /// Whether this selected profile leaves a known Lua builtin absent.
    pub fn is_unavailable(self, name: &str) -> bool {
        GAME_UNAVAILABLE.contains(&name)
            && !(self == Self::Extended && EXTENDED_FUNCTIONS.contains(&name))
    }
    /// Addon-specific base-function availability; Vehicle keeps `select`.
    pub fn is_addon_unavailable(self, name: &str) -> bool {
        self.is_unavailable(name) || (self == Self::Game && name == "select")
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

/// Vehicle callbacks invoked externally, not ordinary dead global functions.
pub const VEHICLE_CALLBACKS: &[&str] = &["onTick", "onDraw", "httpReply"];
