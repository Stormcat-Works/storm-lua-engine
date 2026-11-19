//! Explicit, bounded development controls. They mutate the same state read by standard APIs.
//! Callbacks stay inside the VM; host code never reenters WASM to update an active call.
use super::{read_state, write_state, FastIo, State};
use std::sync::{atomic::Ordering, Arc, RwLock};
use storm_lua_spec::{
    environment::EnvironmentProfile,
    property::{PropertyBag, PropertyValue},
};
use storm_lua_vm::{
    bindings::HostBindings,
    runner::{ErrorKind, VmError},
    value::{HostFunction, LuaValue},
};

const MAX_PROPERTIES: usize = 4096;
const MAX_PROPERTY_BYTES: usize = 1024 * 1024;
fn invalid(message: &str) -> VmError {
    VmError::new(ErrorKind::InvalidArgument, message)
}
fn value_bytes(value: &PropertyValue) -> usize {
    match value {
        PropertyValue::Text(bytes) => bytes.len(),
        _ => 8,
    }
}
fn property_bytes(properties: &PropertyBag) -> usize {
    properties
        .iter()
        .map(|(label, value)| label.len() + value_bytes(value))
        .sum()
}
pub(super) fn validate_properties(properties: &PropertyBag) -> Result<(), VmError> {
    if properties.len() > MAX_PROPERTIES || property_bytes(properties) > MAX_PROPERTY_BYTES {
        return Err(VmError::new(
            ErrorKind::Limit,
            "development properties exceed their entry/byte budget",
        ));
    }
    Ok(())
}
/// 1件を設定したあとの件数とバイト数を、設定する前に検査します。
pub(super) fn validate_property_update(
    properties: &PropertyBag,
    label: &[u8],
    value: &PropertyValue,
) -> Result<(), VmError> {
    let previous = properties.get(label);
    let entries = properties.len() + usize::from(previous.is_none());
    let bytes = property_bytes(properties) - previous.map_or(0, |v| label.len() + value_bytes(v))
        + label.len()
        + value_bytes(value);
    if entries > MAX_PROPERTIES || bytes > MAX_PROPERTY_BYTES {
        return Err(VmError::new(
            ErrorKind::Limit,
            "development properties exceed their entry/byte budget",
        ));
    }
    Ok(())
}
fn index(value: &LuaValue) -> Result<usize, VmError> {
    let channel = match value {
        LuaValue::Integer(n) if (1..=32).contains(n) => *n as usize,
        LuaValue::Number(n) if (1.0..=32.0).contains(n) && n.fract() == 0.0 => *n as usize,
        _ => return Err(invalid("control channel must be an integer in 1..32")),
    };
    Ok(channel - 1)
}
fn numeric(value: &LuaValue) -> Result<f64, VmError> {
    match value {
        LuaValue::Integer(n) => Ok(*n as f64),
        LuaValue::Number(n) => Ok(*n),
        _ => Err(invalid("control value must be a number")),
    }
}
pub(super) fn with_controls(
    base: &HostBindings,
    namespace: Option<&str>,
    environment: EnvironmentProfile,
    io: &Arc<FastIo>,
    state: &Arc<RwLock<State>>,
) -> Result<HostBindings, VmError> {
    let Some(namespace) = namespace else {
        return Ok(base.clone());
    };
    if environment != EnvironmentProfile::Extended {
        return Err(invalid(
            "controlNamespace requires the extended environment",
        ));
    }
    if !storm_lua_spec::environment::valid_binding_path(namespace) || namespace.contains('.') {
        return Err(invalid("controlNamespace must be a root Lua identifier"));
    }
    validate_properties(&read_state(state).properties)?;
    let mut bindings = base.clone();
    let mut add = |name: &str, callback: HostFunction| -> Result<(), VmError> {
        let path = format!("{namespace}.{name}");
        if bindings.functions.contains_key(&path) || bindings.values.contains_key(&path) {
            return Err(invalid("controlNamespace conflicts with a host binding"));
        }
        bindings.functions.insert(path, callback);
        Ok(())
    };
    let s = Arc::clone(state);
    add(
        "setProperty",
        Arc::new(move |args| {
            let [LuaValue::Bytes(label), value] = args else {
                return Err(invalid("setProperty expects a byte-string label and value"));
            };
            let value = match value {
                LuaValue::Nil => {
                    write_state(&s).properties.remove(label);
                    return Ok(vec![]);
                }
                LuaValue::Integer(n) => PropertyValue::Number(*n as f64),
                LuaValue::Number(n) => PropertyValue::Number(*n),
                LuaValue::Bool(b) => PropertyValue::Bool(*b),
                LuaValue::Bytes(bytes) => PropertyValue::Text(bytes.clone()),
                _ => {
                    return Err(invalid(
                        "property value must be number, Boolean, byte string or nil",
                    ))
                }
            };
            let mut state = write_state(&s);
            validate_property_update(&state.properties, label, &value)?;
            state.properties.insert(label.clone(), value);
            Ok(vec![])
        }),
    )?;
    let fast_io = Arc::clone(io);
    add(
        "setInputNumber",
        Arc::new(move |args| {
            let [channel, value] = args else {
                return Err(invalid("setInputNumber expects channel and value"));
            };
            let channel = index(channel)?;
            let value = numeric(value)? as f32;
            fast_io.input_numbers[channel].store(value.to_bits(), Ordering::Relaxed);
            fast_io.input_changed.store(true, Ordering::Relaxed);
            Ok(vec![])
        }),
    )?;
    let fast_io = Arc::clone(io);
    add(
        "setInputBool",
        Arc::new(move |args| {
            let [channel, LuaValue::Bool(value)] = args else {
                return Err(invalid("setInputBool expects channel and Boolean"));
            };
            let channel = index(channel)?;
            fast_io.input_booleans[channel].store(*value, Ordering::Relaxed);
            fast_io.input_changed.store(true, Ordering::Relaxed);
            Ok(vec![])
        }),
    )?;
    let fast_io = Arc::clone(io);
    add(
        "getInputNumber",
        Arc::new(move |args| {
            let [channel] = args else {
                return Err(invalid("getInputNumber expects a channel"));
            };
            Ok(vec![LuaValue::Number(f64::from(f32::from_bits(
                fast_io.input_numbers[index(channel)?].load(Ordering::Relaxed),
            )))])
        }),
    )?;
    let fast_io = Arc::clone(io);
    add(
        "getInputBool",
        Arc::new(move |args| {
            let [channel] = args else {
                return Err(invalid("getInputBool expects a channel"));
            };
            Ok(vec![LuaValue::Bool(
                fast_io.input_booleans[index(channel)?].load(Ordering::Relaxed),
            )])
        }),
    )?;
    bindings.validate(environment)?;
    Ok(bindings)
}
