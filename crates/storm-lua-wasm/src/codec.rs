//! ロスレスなコールドパスJSONコーデック。固定I/Oおよびピクセルデータはこのパスを決して使用しません。
use serde_json::Value;
use storm_lua_bridge::{BridgeError, Status};
use storm_lua_spec::property::{PropertyBag, PropertyValue};

fn invalid(message: &str) -> BridgeError {
    BridgeError::new(Status::InvalidArgument, message)
}
pub(crate) fn parse(bytes: &[u8]) -> Result<Value, BridgeError> {
    if bytes.len() > 4 * 1024 * 1024 {
        return Err(BridgeError::new(
            Status::Limit,
            "control request is too large",
        ));
    }
    serde_json::from_slice(bytes)
        .map_err(|error| invalid(&format!("invalid control JSON: {error}")))
}
pub(crate) fn byte_array(value: &Value) -> Result<Vec<u8>, BridgeError> {
    let values = value
        .as_array()
        .ok_or_else(|| invalid("expected a byte array"))?;
    if values.len() > 1024 * 1024 {
        return Err(BridgeError::new(Status::Limit, "byte field exceeds 1 MiB"));
    }
    values
        .iter()
        .map(|v| {
            v.as_u64()
                .and_then(|v| u8::try_from(v).ok())
                .ok_or_else(|| invalid("invalid byte value"))
        })
        .collect()
}
/// プロパティ1件のwire表現を読みます。一括設定と1件設定で同じ規則を使います。
pub(crate) fn property_entry(entry: &Value) -> Result<(Vec<u8>, PropertyValue), BridgeError> {
    let label = byte_array(&entry["label"])?;
    let value = match entry["kind"].as_str() {
        Some("number") => {
            let bits = entry["bits"]
                .as_str()
                .ok_or_else(|| invalid("number requires IEEE754 hex bits"))?;
            if bits.len() != 16 || !bits.bytes().all(|b| b.is_ascii_hexdigit()) {
                return Err(invalid("invalid f64 bit representation"));
            }
            PropertyValue::Number(f64::from_bits(
                u64::from_str_radix(bits, 16).map_err(|_| invalid("invalid f64 bits"))?,
            ))
        }
        Some("bool") => PropertyValue::Bool(
            entry["value"]
                .as_bool()
                .ok_or_else(|| invalid("Boolean property requires a Boolean value"))?,
        ),
        Some("text") => PropertyValue::Text(byte_array(&entry["bytes"])?),
        _ => return Err(invalid("unknown property kind")),
    };
    Ok((label, value))
}
pub(crate) fn properties(bytes: &[u8]) -> Result<PropertyBag, BridgeError> {
    let root = parse(bytes)?;
    let values = root
        .as_array()
        .ok_or_else(|| invalid("properties must be an entry array"))?;
    if values.len() > 4096 {
        return Err(BridgeError::new(Status::Limit, "too many properties"));
    }
    let mut bag = PropertyBag::default();
    for entry in values {
        let (label, value) = property_entry(entry)?;
        if bag.insert(label, value).is_some() {
            return Err(invalid("duplicate property label"));
        }
    }
    Ok(bag)
}
pub(crate) fn respond(value: &Value) -> Result<(), BridgeError> {
    let bytes = serde_json::to_vec(value)
        .map_err(|error| BridgeError::new(Status::Failed, error.to_string()))?;
    storm_lua_bridge::respond(bytes)
}
#[cfg(feature = "debug")]
pub(crate) fn debug_value(value: storm_lua_vm::debug::DebugValue) -> Value {
    use storm_lua_vm::debug::DebugValue;
    match value {
        DebugValue::Nil => serde_json::json!({"kind":"nil"}),
        DebugValue::Bool(value) => serde_json::json!({"kind":"bool","value":value}),
        DebugValue::Integer(value) => {
            serde_json::json!({"kind":"integer","value":value.to_string()})
        }
        DebugValue::Number(value) => {
            serde_json::json!({"kind":"number","bits":format!("{:016x}",value.to_bits())})
        }
        DebugValue::Bytes(value) => serde_json::json!({"kind":"bytes","value":value}),
        DebugValue::Table(handle) => {
            serde_json::json!({"kind":"table","handle":{"vmId":handle.vm_id.to_string(),"pauseEpoch":handle.pause_epoch.to_string(),"slot":handle.slot}})
        }
        DebugValue::Opaque(kind) => serde_json::json!({"kind":"opaque","typeName":kind}),
    }
}
