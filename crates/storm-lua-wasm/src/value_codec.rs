//! ロスレスなコールドパスのホスト/イベント/セーブデータ用コーデック。通常のComposite ticksでは決してJSONを使用しません。
use serde_json::{json, Value};
use storm_lua_bridge::{BridgeError, Status};
use storm_lua_vm::value::LuaValue;

pub(crate) fn invalid(message: &str) -> BridgeError {
    BridgeError::new(Status::InvalidArgument, message)
}
pub(crate) fn number(value: &Value) -> Result<u32, BridgeError> {
    value
        .as_u64()
        .and_then(|n| u32::try_from(n).ok())
        .ok_or_else(|| invalid("expected a u32"))
}
pub(crate) fn integer(value: &Value) -> Result<i32, BridgeError> {
    value
        .as_i64()
        .and_then(|n| i32::try_from(n).ok())
        .ok_or_else(|| invalid("expected an i32"))
}
pub(crate) fn decimal(value: &Value) -> Result<u64, BridgeError> {
    value
        .as_str()
        .ok_or_else(|| invalid("expected decimal u64 text"))?
        .parse()
        .map_err(|_| invalid("invalid u64 text"))
}
pub(crate) fn encode(value: &LuaValue) -> Value {
    match value {
        LuaValue::Nil => json!({"kind":"nil"}),
        LuaValue::Bool(v) => json!({"kind":"bool","value":v}),
        LuaValue::Integer(v) => json!({"kind":"integer","value":v.to_string()}),
        LuaValue::Number(v) => json!({"kind":"number","bits":format!("{:016x}",v.to_bits())}),
        LuaValue::Bytes(v) => json!({"kind":"bytes","value":v}),
        LuaValue::Table(v) => {
            json!({"kind":"table","entries":v.iter().map(|(k,v)| vec![encode(k),encode(v)]).collect::<Vec<_>>()})
        }
    }
}
#[derive(Default)]
struct Budget {
    nodes: usize,
    bytes: usize,
}
fn decode(value: &Value, budget: &mut Budget, depth: usize) -> Result<LuaValue, BridgeError> {
    budget.nodes += 1;
    if depth > 32 || budget.nodes > 65536 {
        return Err(BridgeError::new(
            Status::Limit,
            "host value exceeds depth/node budget",
        ));
    }
    Ok(match value["kind"].as_str() {
        Some("nil") => LuaValue::Nil,
        Some("bool") => LuaValue::Bool(
            value["value"]
                .as_bool()
                .ok_or_else(|| invalid("invalid Boolean value"))?,
        ),
        Some("integer") => LuaValue::Integer(
            value["value"]
                .as_str()
                .ok_or_else(|| invalid("integer requires decimal text"))?
                .parse()
                .map_err(|_| invalid("invalid signed i64"))?,
        ),
        Some("number") => {
            let bits = value["bits"]
                .as_str()
                .ok_or_else(|| invalid("number requires IEEE754 hex bits"))?;
            if bits.len() != 16 || !bits.bytes().all(|b| b.is_ascii_hexdigit()) {
                return Err(invalid("invalid binary64 bits"));
            }
            LuaValue::Number(f64::from_bits(
                u64::from_str_radix(bits, 16).map_err(|_| invalid("invalid binary64 bits"))?,
            ))
        }
        Some("bytes") => {
            let bytes = crate::codec::byte_array(&value["value"])?;
            budget.bytes += bytes.len();
            if budget.bytes > 1024 * 1024 {
                return Err(BridgeError::new(
                    Status::Limit,
                    "host value byte budget exceeded",
                ));
            }
            LuaValue::Bytes(bytes)
        }
        Some("table") => {
            let entries = value["entries"]
                .as_array()
                .ok_or_else(|| invalid("table requires entry pairs"))?;
            if entries.len() > 32768 {
                return Err(BridgeError::new(Status::Limit, "too many table entries"));
            }
            let mut result = Vec::new();
            for entry in entries {
                let pair = entry
                    .as_array()
                    .filter(|v| v.len() == 2)
                    .ok_or_else(|| invalid("invalid table pair"))?;
                result.push((
                    decode(&pair[0], budget, depth + 1)?,
                    decode(&pair[1], budget, depth + 1)?,
                ));
            }
            LuaValue::Table(result)
        }
        _ => return Err(invalid("unknown host value kind")),
    })
}
pub(crate) fn value(value: &Value) -> Result<LuaValue, BridgeError> {
    decode(value, &mut Budget::default(), 0)
}
pub(crate) fn values(value: &Value) -> Result<Vec<LuaValue>, BridgeError> {
    let list = value
        .as_array()
        .ok_or_else(|| invalid("expected an argument/result list"))?;
    if list.len() > 65536 {
        return Err(BridgeError::new(
            Status::Limit,
            "too many arguments/results",
        ));
    }
    let mut budget = Budget::default();
    list.iter().map(|v| decode(v, &mut budget, 0)).collect()
}
