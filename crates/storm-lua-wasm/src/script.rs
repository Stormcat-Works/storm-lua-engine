//! 単一の世代付きハンドルレジストリ内における、型付きスクリプト判別子。
use storm_lua_addon::Addon;
use storm_lua_bridge::{BridgeError, Status};
use storm_lua_component::Component;
use storm_lua_microcontroller::Microcontroller;
use storm_lua_spec::http::{HttpRequest, HttpToken};
use storm_lua_vm::{
    logging::LogRecord,
    runner::{ErrorKind, RunOutcome, VmError},
};
pub(crate) enum Script {
    Vehicle(Box<Microcontroller>),
    Addon(Box<Addon>),
    Component(Box<Component>),
}
impl Script {
    pub(crate) fn vehicle(&mut self) -> Result<&mut Microcontroller, BridgeError> {
        match self {
            Self::Vehicle(vm) => Ok(vm),
            Self::Addon(_) | Self::Component(_) => Err(BridgeError::new(
                Status::InvalidArgument,
                "this operation requires vehicle mode",
            )),
        }
    }
    pub(crate) fn addon(&mut self) -> Result<&mut Addon, BridgeError> {
        match self {
            Self::Addon(vm) => Ok(vm),
            Self::Vehicle(_) | Self::Component(_) => Err(BridgeError::new(
                Status::InvalidArgument,
                "this operation requires addon mode",
            )),
        }
    }
    pub(crate) fn component(&mut self) -> Result<&mut Component, BridgeError> {
        match self {
            Self::Component(vm) => Ok(vm),
            Self::Vehicle(_) | Self::Addon(_) => Err(BridgeError::new(
                Status::InvalidArgument,
                "this operation requires component mode",
            )),
        }
    }
    pub(crate) fn load(&mut self, source: &[u8], name: &str) -> Result<RunOutcome, VmError> {
        match self {
            Self::Vehicle(vm) => vm.load(source, name),
            Self::Addon(vm) => vm.load(source, name),
            Self::Component(vm) => vm.load(source, name),
        }
    }
    pub(crate) fn is_suspended(&self) -> bool {
        match self {
            Self::Vehicle(vm) => vm.is_suspended(),
            Self::Addon(vm) => vm.is_suspended(),
            Self::Component(vm) => vm.is_suspended(),
        }
    }
    pub(crate) fn is_failed(&self) -> bool {
        match self {
            Self::Vehicle(vm) => vm.is_failed(),
            Self::Addon(vm) => vm.is_failed(),
            Self::Component(vm) => vm.is_failed(),
        }
    }
    pub(crate) fn enable_dev_logs(&mut self) -> Result<(), VmError> {
        match self {
            Self::Vehicle(vm) => vm.enable_dev_logs(),
            Self::Addon(vm) => vm.enable_dev_logs(),
            Self::Component(vm) => vm.enable_dev_logs(),
        }
    }
    pub(crate) fn drain_log_records(&mut self) -> Vec<LogRecord> {
        match self {
            Self::Vehicle(vm) => vm.drain_log_records(),
            Self::Addon(vm) => vm.drain_log_records(),
            Self::Component(vm) => vm.drain_log_records(),
        }
    }
    pub(crate) fn drain_http_requests(&mut self) -> Vec<HttpRequest> {
        match self {
            Self::Vehicle(vm) => vm.drain_http_requests(),
            Self::Addon(vm) => vm.drain_http_requests(),
            // ComponentプロファイルにはHTTP API自体が無いため、保留中の
            // リクエストは存在し得ず、空が事実どおりの結果です。
            Self::Component(_) => Vec::new(),
        }
    }
    pub(crate) fn http_reply(
        &mut self,
        token: HttpToken,
        bytes: &[u8],
    ) -> Result<RunOutcome, VmError> {
        match self {
            Self::Vehicle(vm) => vm.http_reply(token, bytes),
            Self::Addon(vm) => vm.http_reply(token, bytes),
            Self::Component(_) => Err(VmError::new(
                ErrorKind::Unsupported,
                "component profile does not provide HTTP",
            )),
        }
    }
    pub(crate) fn cancel_http(&mut self, token: HttpToken) -> Result<(), VmError> {
        match self {
            Self::Vehicle(vm) => vm.cancel_http(token),
            Self::Addon(vm) => vm.cancel_http(token),
            Self::Component(_) => Err(VmError::new(
                ErrorKind::Unsupported,
                "component profile does not provide HTTP",
            )),
        }
    }
}
#[cfg(feature = "debug")]
use storm_lua_vm::{
    debug::{DebugHandle, DebugValue, StackFrame, TableEntry, Variable},
    runner::StepMode,
};
#[cfg(feature = "debug")]
impl Script {
    pub(crate) fn set_breakpoints(&mut self, points: Vec<(String, i32)>) -> Result<(), VmError> {
        match self {
            Self::Vehicle(vm) => vm.set_breakpoints(points),
            Self::Addon(vm) => vm.set_breakpoints(points),
            Self::Component(vm) => vm.set_breakpoints(points),
        }
    }
    pub(crate) fn resume_with(
        &mut self,
        mode: StepMode,
        carry: bool,
    ) -> Result<RunOutcome, VmError> {
        match self {
            Self::Vehicle(vm) => vm.resume_with(mode, carry),
            Self::Addon(vm) => vm.resume_with(mode, carry),
            Self::Component(vm) => vm.resume_with(mode, carry),
        }
    }
    pub(crate) fn request_pause(&mut self) -> Result<(), VmError> {
        match self {
            Self::Vehicle(vm) => vm.request_pause(),
            Self::Addon(vm) => vm.request_pause(),
            Self::Component(vm) => vm.request_pause(),
        }
    }
    pub(crate) fn cancel_pause(&mut self) {
        match self {
            Self::Vehicle(vm) => vm.cancel_pause(),
            Self::Addon(vm) => vm.cancel_pause(),
            Self::Component(vm) => vm.cancel_pause(),
        }
    }
    pub(crate) fn breakable_lines(&self, source: &[u8], name: &str) -> Result<Vec<u32>, VmError> {
        match self {
            Self::Vehicle(vm) => vm.breakable_lines(source, name),
            Self::Addon(vm) => vm.breakable_lines(source, name),
            Self::Component(vm) => vm.breakable_lines(source, name),
        }
    }
    pub(crate) fn stack(&self) -> Result<Vec<StackFrame>, VmError> {
        match self {
            Self::Vehicle(vm) => vm.stack(),
            Self::Addon(vm) => vm.stack(),
            Self::Component(vm) => vm.stack(),
        }
    }
    pub(crate) fn locals(&mut self, level: u32) -> Result<Vec<Variable>, VmError> {
        match self {
            Self::Vehicle(vm) => vm.locals(level),
            Self::Addon(vm) => vm.locals(level),
            Self::Component(vm) => vm.locals(level),
        }
    }
    pub(crate) fn upvalues(&mut self, level: u32) -> Result<Vec<Variable>, VmError> {
        match self {
            Self::Vehicle(vm) => vm.upvalues(level),
            Self::Addon(vm) => vm.upvalues(level),
            Self::Component(vm) => vm.upvalues(level),
        }
    }
    pub(crate) fn expand_table(
        &mut self,
        handle: DebugHandle,
        start: usize,
        limit: usize,
    ) -> Result<Vec<TableEntry>, VmError> {
        match self {
            Self::Vehicle(vm) => vm.expand_table(handle, start, limit),
            Self::Addon(vm) => vm.expand_table(handle, start, limit),
            Self::Component(vm) => vm.expand_table(handle, start, limit),
        }
    }
    pub(crate) fn evaluate_watch(
        &mut self,
        level: u32,
        expression: &str,
    ) -> Result<DebugValue, VmError> {
        match self {
            Self::Vehicle(vm) => vm.evaluate_watch(level, expression),
            Self::Addon(vm) => vm.evaluate_watch(level, expression),
            Self::Component(vm) => vm.evaluate_watch(level, expression),
        }
    }
}
