//! Send/Sync regression probe for the engine handle types converted for the
//! Send wave (Route B). This pins the *type* contract only: it never executes
//! Lua and never moves a live handle to another thread. Each assertion is a
//! compile-time bound, so a regression that reintroduces an `Rc`/`RefCell`
//! leaf fails this test file's build.
#![allow(dead_code)]

use storm_lua_addon::{Addon, AddonCallback, AddonConfig, SynchronousEventPass, SynchronousEvents};
use storm_lua_component::Component;
use storm_lua_microcontroller::Microcontroller;
use storm_lua_vm::{
    bindings::HostBindings,
    host::HostCallContext,
    logging::LogReader,
    random::SharedCrtRandom,
    runner::{SynchronousCallbackPass, SynchronousCallbacks, Vm},
    source::RequireLoader,
    value::HostFunction,
};
use storm_screen_raster::raster::ScreenRaster;

fn require_send<T: Send>() {}
fn require_send_sync<T: Send + Sync>() {}
fn require_sync<T: Sync>() {}

#[test]
fn engine_handle_types_are_send() {
    // VM and its shared handles.
    require_send::<Vm>();
    require_send::<HostCallContext>();
    require_send::<LogReader>();
    require_send::<SynchronousCallbacks>();
    require_send::<SynchronousCallbackPass>();
    // Engine owner types.
    require_send::<Addon>();
    require_send::<AddonConfig>();
    require_send::<AddonCallback>();
    require_send::<SynchronousEvents>();
    require_send::<SynchronousEventPass>();
    require_send::<Microcontroller>();
    require_send::<Component>();
    require_send::<ScreenRaster>();
    // Host-supplied extension types.
    require_send::<HostBindings>();
    require_send::<HostFunction>();
    require_send::<SharedCrtRandom>();
    require_send::<RequireLoader>();
}

#[test]
fn host_handles_that_hold_no_lua_state_are_also_sync() {
    // These carry only atomic/mutex-guarded data or `mlua` handles whose Lua
    // state is not touched by the host adapter.
    require_sync::<HostBindings>();
    require_send_sync::<HostFunction>();
    require_send_sync::<SharedCrtRandom>();
    require_send_sync::<RequireLoader>();
}
