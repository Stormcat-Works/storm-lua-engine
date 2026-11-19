//! Explicit trusted-host bindings applied before user source is loaded.
use crate::{
    runner::{ErrorKind, Vm, VmError},
    value::{HostFunction, LuaValue},
};
use mlua::{MultiValue, Table, Value};
use std::collections::{BTreeMap, BTreeSet};
use storm_lua_spec::environment::{valid_binding_path, EnvironmentProfile};

/// Host data and synchronous functions. Paths can name a global or a table member.
/// Functions/threads/userdata do not cross the owned-value callback boundary.
#[derive(Clone, Default)]
pub struct HostBindings {
    /// Explicit values; `LuaValue::Nil` removes a binding.
    pub values: BTreeMap<String, LuaValue>,
    /// Explicit functions, including replacements for existing builtin functions.
    pub functions: BTreeMap<String, HostFunction>,
    /// Snapshot-only lookup paths after a trusted host bootstrap moves bindings.
    /// Keys are the original registered paths; values are their live capture paths.
    /// Images retain the original symbols and restore against freshly seeded bindings,
    /// before heap reconciliation reinstates the captured namespace layout.
    /// This does not install/expose bindings or execute code, and is allowed in Game.
    pub snapshot_capture_paths: BTreeMap<Vec<String>, Vec<String>>,
}
impl std::fmt::Debug for HostBindings {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HostBindings")
            .field("values", &self.values)
            .field("functions", &self.functions.keys().collect::<Vec<_>>())
            .field("snapshot_capture_paths", &self.snapshot_capture_paths)
            .finish()
    }
}
impl HostBindings {
    /// Whether no host bindings or snapshot lookup configuration are supplied.
    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
            && self.functions.is_empty()
            && self.snapshot_capture_paths.is_empty()
    }
    /// Reject conflicting paths before any user code or host callback is executed.
    pub fn validate(&self, profile: EnvironmentProfile) -> Result<(), VmError> {
        if (!self.values.is_empty() || !self.functions.is_empty())
            && profile != EnvironmentProfile::Extended
        {
            return Err(VmError::new(
                ErrorKind::InvalidArgument,
                "host bindings require the extended environment",
            ));
        }
        if self.snapshot_capture_paths.len() > 512 {
            return Err(VmError::new(
                ErrorKind::Limit,
                "too many snapshot capture paths",
            ));
        }
        for (registered, live) in &self.snapshot_capture_paths {
            for path in [registered, live] {
                if path.is_empty() || path.iter().any(String::is_empty) {
                    return Err(VmError::new(
                        ErrorKind::InvalidArgument,
                        "snapshot capture paths must have non-empty segments",
                    ));
                }
            }
        }
        let mut seen = BTreeSet::new();
        for path in self.values.keys().chain(self.functions.keys()) {
            if !valid_binding_path(path) || !seen.insert(path.clone()) {
                return Err(VmError::new(
                    ErrorKind::InvalidArgument,
                    "invalid or duplicate host binding path",
                ));
            }
        }
        if seen.len() > 512 {
            return Err(VmError::new(ErrorKind::Limit, "too many host bindings"));
        }
        for path in &seen {
            let mut parent = path.as_str();
            while let Some((head, _)) = parent.rsplit_once('.') {
                if seen.contains(head) {
                    return Err(VmError::new(
                        ErrorKind::InvalidArgument,
                        "host binding paths overlap",
                    ));
                }
                parent = head;
            }
        }
        Ok(())
    }
}
impl Vm {
    /// Apply explicit host bindings while idle. High-level profiles preserve them across reset/reload.
    pub fn install_bindings(&mut self, bindings: &HostBindings) -> Result<(), VmError> {
        self.ensure_idle()?;
        bindings.validate(self.environment_profile)?;
        for (path, value) in &bindings.values {
            let mut encoded = crate::value::encode(&self.lua, std::slice::from_ref(value))?;
            let encoded = encoded.pop_front().ok_or_else(|| {
                VmError::new(ErrorKind::InvalidArgument, "missing encoded host value")
            })?;
            self.set_path(path, encoded)?;
            let segments: Vec<String> = path.split('.').map(str::to_owned).collect();
            for prefix_len in 1..segments.len() {
                self.note_host_path(segments[..prefix_len].to_vec());
            }
            self.unregister_host_prefix(&segments);
            if matches!(value, LuaValue::Table(_)) {
                self.note_host_path(segments);
            }
        }
        for (path, handler) in &bindings.functions {
            let handler = std::sync::Arc::clone(handler);
            let function = self.lua.create_function(move |lua, args: MultiValue| {
                let args = crate::value::decode(args).map_err(mlua::Error::external)?;
                let result = handler(&args).map_err(mlua::Error::external)?;
                crate::value::encode(lua, &result).map_err(mlua::Error::external)
            })?;
            self.set_path(path, Value::Function(function))?;
            let segments: Vec<String> = path.split('.').map(str::to_owned).collect();
            self.unregister_host_prefix(&segments);
            for prefix_len in 1..=segments.len() {
                self.note_host_path(segments[..prefix_len].to_vec());
            }
        }
        self.snapshot_capture_paths = bindings.snapshot_capture_paths.clone();
        Ok(())
    }
    fn set_path(&self, path: &str, value: Value) -> Result<(), VmError> {
        let mut table = self.environment.clone();
        let mut parts = path.split('.').peekable();
        while let Some(part) = parts.next() {
            if parts.peek().is_none() {
                table.raw_set(part, value)?;
                return Ok(());
            }
            table = match table.raw_get::<Value>(part)? {
                Value::Table(next) => next,
                Value::Nil => {
                    let next: Table = self.lua.create_table()?;
                    table.raw_set(part, next.clone())?;
                    next
                }
                _ => {
                    return Err(VmError::new(
                        ErrorKind::InvalidArgument,
                        "host binding parent is not a table",
                    ))
                }
            };
        }
        Err(VmError::new(
            ErrorKind::InvalidArgument,
            "empty host binding path",
        ))
    }
}
