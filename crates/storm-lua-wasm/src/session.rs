//! アダプタが所有するI/Oおよびラスタライザの状態。単一のレジストリによりモード間でのハンドルの衝突を防ぎます。
use crate::script::Script;
use std::{cell::RefCell, rc::Rc};
use storm_lua_bridge::{BridgeError, Registry, Status};
use storm_lua_microcontroller::{Microcontroller, MicrocontrollerConfig};
use storm_lua_spec::{
    abi::IoBuffer,
    draw::{ScreenError, ScreenSink},
    io::CompositeSignal,
    map::{MapProvider, MapRequest},
};
use storm_lua_vm::{
    runner::{ErrorKind, RunOutcome, VmError},
    ExecutionLimits,
};
use storm_screen_raster::raster::ScreenRaster;

pub(crate) struct Session {
    pub(crate) vm: Script,
    pub(crate) io: Option<Box<IoBuffer>>,
    pub(crate) raster: Option<ScreenRaster>,
    pub(crate) epoch: u32,
    pub(crate) replayed: usize,
    pub(crate) drawing: bool,
    map_host: Option<u32>,
}
thread_local! {static SESSIONS:RefCell<Registry<Session>>=RefCell::new(Registry::default());}
pub(crate) fn convert(error: VmError) -> BridgeError {
    BridgeError::new(
        match error.kind {
            ErrorKind::Busy => Status::Busy,
            ErrorKind::Failed => Status::Failed,
            ErrorKind::Limit => Status::Limit,
            ErrorKind::Lua => Status::Lua,
            ErrorKind::InvalidArgument => Status::InvalidArgument,
            ErrorKind::Unsupported => Status::Unsupported,
            ErrorKind::Host => Status::Host,
        },
        error.to_string(),
    )
}
pub(crate) fn outcome(value: RunOutcome) -> Status {
    match value {
        RunOutcome::Completed => Status::Ok,
        RunOutcome::Suspended => Status::Suspended,
        RunOutcome::Missing => Status::Missing,
    }
}
pub(crate) fn limits(instructions: u32, memory: u32) -> Result<ExecutionLimits, BridgeError> {
    let instruction_budget =
        std::num::NonZeroU64::new(u64::from(instructions)).ok_or_else(|| {
            BridgeError::new(
                Status::InvalidArgument,
                "instruction budget must be nonzero",
            )
        })?;
    let lua_memory_bytes = std::num::NonZeroUsize::new(memory as usize).ok_or_else(|| {
        BridgeError::new(Status::InvalidArgument, "memory budget must be nonzero")
    })?;
    if memory > 256 * 1024 * 1024 {
        return Err(BridgeError::new(
            Status::InvalidArgument,
            "Lua memory limit exceeds adapter maximum",
        ));
    }
    Ok(ExecutionLimits {
        instruction_budget,
        lua_memory_bytes,
    })
}
pub(crate) fn insert(vm: Script) -> Result<usize, BridgeError> {
    let io = if matches!(&vm, Script::Vehicle(_)) {
        Some(Box::default())
    } else {
        None
    };
    SESSIONS.with(|registry| {
        let mut registry = registry
            .try_borrow_mut()
            .map_err(|_| BridgeError::new(Status::Busy, "session registry is busy"))?;
        Ok(registry.insert(Session {
            vm,
            io,
            raster: None,
            epoch: 1,
            replayed: 0,
            drawing: false,
            map_host: None,
        })? as usize)
    })
}
pub(crate) fn create(instructions: u32, memory: u32) -> Result<usize, BridgeError> {
    let vm = Microcontroller::new(MicrocontrollerConfig {
        limits: limits(instructions, memory)?,
        ..Default::default()
    })
    .map_err(convert)?;
    insert(Script::Vehicle(Box::new(vm)))
}
pub(crate) fn with<T>(
    handle: u32,
    action: impl FnOnce(&mut Session) -> Result<T, BridgeError>,
) -> Result<T, BridgeError> {
    SESSIONS.with(|registry| {
        let mut registry = registry
            .try_borrow_mut()
            .map_err(|_| BridgeError::new(Status::Busy, "session registry is busy"))?;
        action(registry.get_mut(handle)?)
    })
}
pub(crate) fn dispose(handle: u32) -> Result<Status, BridgeError> {
    SESSIONS.with(|registry| {
        registry
            .try_borrow_mut()
            .map_err(|_| BridgeError::new(Status::Busy, "session registry is busy"))?
            .remove(handle)?;
        Ok(Status::Ok)
    })
}
fn map_provider(key: u32) -> Rc<dyn MapProvider> {
    Rc::new(move |request: &MapRequest| {
        let value = serde_json::json!({"kind":"map","width":request.width,"height":request.height,"center":request.center,"zoom":request.zoom,"colors":request.colors.map(|color|color.map(|c|c.0))});
        crate::host::call(key, &value).map_err(|e| ScreenError::Host(e.message))
    })
}
impl Session {
    pub(crate) fn idle(&self) -> Result<(), BridgeError> {
        if self.vm.is_suspended() {
            Err(BridgeError::new(Status::Busy, "callback is suspended"))
        } else if self.vm.is_failed() {
            Err(BridgeError::new(
                Status::Failed,
                "recreate the VM after failure",
            ))
        } else {
            Ok(())
        }
    }
    pub(crate) fn advance_epoch(&mut self) -> Result<(), BridgeError> {
        self.epoch = self
            .epoch
            .checked_add(1)
            .ok_or_else(|| BridgeError::new(Status::Limit, "frame epoch exhausted"))?;
        Ok(())
    }
    pub(crate) fn sync_output(&mut self) {
        if let (Script::Vehicle(vm), Some(io)) = (&mut self.vm, &mut self.io) {
            if let Some(input) = vm.take_input_changes() {
                io.input_numbers = input.numbers;
                io.input_booleans = input.booleans.map(u8::from);
            }
            let signal = vm.output();
            io.output_numbers = signal.numbers;
            for (dst, src) in io.output_booleans.iter_mut().zip(signal.booleans) {
                *dst = u8::from(src);
            }
        }
    }
    pub(crate) fn io_ptr(&mut self) -> Result<usize, BridgeError> {
        let io = self.io.as_mut().ok_or_else(|| {
            BridgeError::new(Status::InvalidArgument, "addon mode has no Composite I/O")
        })?;
        Ok((&mut **io as *mut IoBuffer) as usize)
    }
    pub(crate) fn set_map_host(&mut self, key: u32) -> Result<Status, BridgeError> {
        self.idle()?;
        self.vm.vehicle()?;
        self.map_host = if key == 0 { None } else { Some(key) };
        if let Some(raster) = &mut self.raster {
            raster.set_map_provider(self.map_host.map(map_provider));
        }
        Ok(Status::Ok)
    }
    pub(crate) fn tick(&mut self) -> Result<Status, BridgeError> {
        self.call_tick("onTick", &[])
    }
    pub(crate) fn call_tick(
        &mut self,
        name: &str,
        arguments: &[storm_lua_vm::value::LuaValue],
    ) -> Result<Status, BridgeError> {
        self.idle()?;
        let io = self.io.as_ref().ok_or_else(|| {
            BridgeError::new(
                Status::InvalidArgument,
                "use addon.tick(gameTicks) for addon mode",
            )
        })?;
        if io.input_booleans.iter().any(|v| *v > 1) {
            return Err(BridgeError::new(
                Status::InvalidArgument,
                "Boolean input must be 0 or 1",
            ));
        }
        let input = CompositeSignal {
            numbers: io.input_numbers,
            booleans: io.input_booleans.map(|v| v == 1),
        };
        self.drawing = false;
        let result = self.vm.vehicle()?.call_tick(name, arguments, &input);
        self.sync_output();
        result.map(outcome).map_err(convert)
    }
    pub(crate) fn draw(&mut self, width: u32, height: u32) -> Result<Status, BridgeError> {
        self.call_draw("onDraw", &[], width, height)
    }
    pub(crate) fn call_draw(
        &mut self,
        name: &str,
        arguments: &[storm_lua_vm::value::LuaValue],
        width: u32,
        height: u32,
    ) -> Result<Status, BridgeError> {
        self.idle()?;
        self.vm.vehicle()?;
        self.advance_epoch()?;
        if self
            .raster
            .as_ref()
            .is_none_or(|r| r.dimensions() != (width, height))
        {
            let mut raster = ScreenRaster::new(width, height)?;
            raster.set_map_provider(self.map_host.map(map_provider));
            self.raster = Some(raster);
        }
        let raster = self
            .raster
            .as_mut()
            .ok_or_else(|| BridgeError::new(Status::Failed, "missing raster"))?;
        raster.begin_frame();
        self.replayed = 0;
        self.drawing = true;
        let result = self.vm.vehicle()?.call_draw(name, arguments, width, height);
        self.sync_output();
        let replay = self.refresh_frame();
        match result {
            Err(e) => Err(convert(e)),
            Ok(value) => {
                replay?;
                Ok(outcome(value))
            }
        }
    }
    pub(crate) fn refresh_frame(&mut self) -> Result<(), BridgeError> {
        if !self.drawing {
            return Ok(());
        }
        if let (Some(raster), Script::Vehicle(vm)) = (&mut self.raster, &self.vm) {
            let commands = vm.commands();
            for command in commands.iter().skip(self.replayed) {
                raster.submit(command)?;
                self.replayed += 1;
            }
        }
        Ok(())
    }
    pub(crate) fn reset(&mut self) -> Result<Status, BridgeError> {
        self.vm.vehicle()?.reset().map_err(convert)?;
        self.advance_epoch()?;
        self.raster = None;
        self.replayed = 0;
        self.drawing = false;
        if let Some(io) = &mut self.io {
            **io = IoBuffer::default();
        }
        self.sync_output();
        Ok(Status::Ok)
    }
    #[cfg(feature = "debug")]
    pub(crate) fn resume(
        &mut self,
        mode: storm_lua_vm::runner::StepMode,
    ) -> Result<Status, BridgeError> {
        self.advance_epoch()?;
        let result = self.vm.resume(mode);
        self.sync_output();
        let replay = self.refresh_frame();
        match result {
            Err(e) => Err(convert(e)),
            Ok(value) => {
                replay?;
                Ok(outcome(value))
            }
        }
    }
}
