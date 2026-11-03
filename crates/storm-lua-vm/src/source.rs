//! Host-provided text chunks and explicitly enabled include-once loading.
//! The loader compiles a function in Rust, then returns it to a Lua wrapper.
//! Executing that function from Lua (not inside the Rust callback) keeps the
//! caller's coroutine, instruction budget and debugger continuation intact.
use crate::{
    bindings::HostBindings,
    runner::{ErrorKind, Vm, VmError},
};
use mlua::{ChunkMode, Function, Value};
use std::{cell::Cell, fmt, rc::Rc};
use storm_lua_spec::environment::EnvironmentProfile;

/// Maximum source bytes in one user/required chunk.
pub const MAX_SOURCE_BYTES: usize = 1024 * 1024;
/// Maximum combined retained source bytes or resolved source bytes per VM.
pub const MAX_PROGRAM_BYTES: usize = 8 * 1024 * 1024;
/// Maximum number of explicitly loaded chunks retained for Vehicle reset.
pub const MAX_PROGRAM_CHUNKS: usize = 128;
const MAX_REQUIRED_CHUNKS: usize = 1024;

/// An owned text chunk with a stable debugger source name, such as `@lib/math.lua`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceChunk {
    /// Text Lua bytes; binary bytecode is never accepted.
    pub source: Vec<u8>,
    /// Chunk identity for errors and source-qualified breakpoints, not a file to open.
    pub name: String,
}
impl SourceChunk {
    /// Validate host size/name boundaries before compiling or executing the chunk.
    pub fn validate(&self) -> Result<(), VmError> {
        validate_source(&self.source, &self.name)
    }
}
/// Validate the borrowed form without allocating or touching Lua state.
pub fn validate_source(source: &[u8], name: &str) -> Result<(), VmError> {
    if source.len() > MAX_SOURCE_BYTES {
        return Err(VmError::new(ErrorKind::Limit, "Lua source exceeds 1 MiB"));
    }
    if name.is_empty() || name.len() > 1024 || name.contains('\0') {
        return Err(VmError::new(
            ErrorKind::InvalidArgument,
            "invalid Lua chunk name",
        ));
    }
    Ok(())
}

type ResolveSource = dyn Fn(&str) -> Result<SourceChunk, VmError>;
/// Synchronous source supplier for the development-only include-once `require`.
/// This is not package.loaded/module-return-value require. The host owns path
/// resolution and supplies source, never a Lua Function or a recursive VM call.
#[derive(Clone)]
pub struct RequireLoader(Rc<ResolveSource>);
impl fmt::Debug for RequireLoader {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("RequireLoader(host)")
    }
}
impl RequireLoader {
    /// Register a synchronous resolver. Missing sources must be reported as errors.
    pub fn new(resolve: impl Fn(&str) -> Result<SourceChunk, VmError> + 'static) -> Self {
        Self(Rc::new(resolve))
    }
    /// Reject ambiguous ownership of require before any user source is executed.
    pub fn validate_configuration(
        &self,
        environment: EnvironmentProfile,
        bindings: &HostBindings,
    ) -> Result<(), VmError> {
        if environment != EnvironmentProfile::Extended {
            return Err(VmError::new(
                ErrorKind::InvalidArgument,
                "requireLoader requires the extended environment",
            ));
        }
        if bindings
            .values
            .keys()
            .chain(bindings.functions.keys())
            .any(|path| path == "require" || path.starts_with("require."))
        {
            return Err(VmError::new(
                ErrorKind::InvalidArgument,
                "requireLoader conflicts with require bindings",
            ));
        }
        Ok(())
    }
}
impl Vm {
    /// Install an include-once loader while idle, normally during VM construction.
    /// Each logical name executes at most once, using the shared script environment
    /// and separate chunk locals. Return values are discarded. A runtime failure
    /// after execution began remains cached, matching LifeBoat's include semantics.
    pub fn install_require_loader(&mut self, loader: &RequireLoader) -> Result<(), VmError> {
        self.ensure_idle()?;
        loader.validate_configuration(self.environment_profile, &HostBindings::default())?;
        if !matches!(self.environment.raw_get::<Value>("require")?, Value::Nil) {
            return Err(VmError::new(
                ErrorKind::InvalidArgument,
                "require is already installed",
            ));
        }
        let environment = self.environment.clone();
        let resolve = Rc::clone(&loader.0);
        let count = Cell::new(0usize);
        let bytes = Cell::new(0usize);
        let compile = self.lua.create_function(move |lua, name: Value| {
            let name = match name {
                Value::String(value) => value.to_str()?.to_owned(),
                _ => {
                    return Err(mlua::Error::external(VmError::new(
                        ErrorKind::InvalidArgument,
                        "require name must be a UTF-8 string",
                    )))
                }
            };
            if name.is_empty() || name.len() > 1024 || name.contains('\0') {
                return Err(mlua::Error::external(VmError::new(
                    ErrorKind::InvalidArgument,
                    "invalid require name",
                )));
            }
            if count.get() >= MAX_REQUIRED_CHUNKS {
                return Err(mlua::Error::external(VmError::new(
                    ErrorKind::Limit,
                    "required chunk limit exceeded",
                )));
            }
            let chunk = resolve(&name).map_err(mlua::Error::external)?;
            chunk.validate().map_err(mlua::Error::external)?;
            if chunk.source.len() > MAX_PROGRAM_BYTES - bytes.get() {
                return Err(mlua::Error::external(VmError::new(
                    ErrorKind::Limit,
                    "required sources exceed 8 MiB",
                )));
            }
            let source = crate::dialect::rewrite(&chunk.source)
                .map_err(|message| mlua::Error::external(VmError::new(ErrorKind::Lua, message)))?;
            if source.len() > MAX_PROGRAM_BYTES - bytes.get() {
                return Err(mlua::Error::external(VmError::new(
                    ErrorKind::Limit,
                    "required sources exceed 8 MiB",
                )));
            }
            // No user code runs across this C callback. A Lua caller invokes the
            // returned function after the callback returns, allowing debug yield.
            let function = lua
                .load(source.as_ref())
                .set_name(&chunk.name)
                .set_mode(ChunkMode::Text)
                .set_environment(environment.clone())
                .into_function()?;
            count.set(count.get() + 1);
            bytes.set(bytes.get() + chunk.source.len());
            Ok(function)
        })?;
        let factory = self
            .lua
            .load(include_str!("require.lua"))
            .set_name("=engine:require")
            .set_mode(ChunkMode::Text)
            .set_environment(self.environment.clone())
            .into_function()?;
        let function: Function = factory.call(compile)?;
        self.environment.raw_set("require", function)?;
        Ok(())
    }
}
