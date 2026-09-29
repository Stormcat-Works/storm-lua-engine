//! アドオン、ログ、HTTPのコールドパス要求。ゲームセマンティクスは各プロファイルの所有元に留まります。
use crate::{
    codec, host,
    script::Script,
    session::{self, convert, outcome},
    value_codec::{self, decimal, invalid, number},
};
use serde_json::{json, Value};
use std::{collections::BTreeMap, rc::Rc};
use storm_lua_addon::{Addon, AddonConfig, MenuProperty};
use storm_lua_bridge::{BridgeError, Status};
use storm_lua_spec::http::HttpToken;
use storm_lua_vm::{
    logging::LogSource,
    runner::{ErrorKind, VmError},
    value::HostFunction,
};
fn host_error(error: BridgeError) -> VmError {
    VmError::new(
        match error.status {
            Status::Busy => ErrorKind::Busy,
            Status::Limit => ErrorKind::Limit,
            Status::Unsupported => ErrorKind::Unsupported,
            _ => ErrorKind::Host,
        },
        error.message,
    )
}
fn environment(
    request: &Value,
) -> Result<storm_lua_spec::environment::EnvironmentProfile, BridgeError> {
    match request.get("environment") {
        None => Ok(Default::default()),
        Some(value) => serde_json::from_value(value.clone()).map_err(|e| invalid(&e.to_string())),
    }
}
fn bindings(
    request: &Value,
    host_key: u32,
) -> Result<storm_lua_vm::bindings::HostBindings, BridgeError> {
    let mut result = storm_lua_vm::bindings::HostBindings::default();
    let Some(input) = request.get("bindings") else {
        return Ok(result);
    };
    if !input.is_object() {
        return Err(invalid("bindings must be an object"));
    }
    if let Some(values) = input.get("values") {
        for (path, value) in values
            .as_object()
            .ok_or_else(|| invalid("binding values must be an object"))?
        {
            result
                .values
                .insert(path.clone(), value_codec::value(value)?);
        }
    }
    if let Some(functions) = input.get("functions") {
        for path in functions
            .as_array()
            .ok_or_else(|| invalid("binding functions must be a list"))?
        {
            let path = path
                .as_str()
                .ok_or_else(|| invalid("binding path must be text"))?
                .to_owned();
            if host_key == 0 {
                return Err(invalid("missing host binding provider"));
            }
            let name = path.clone();
            let function: HostFunction = Rc::new(move |args| {
                let request = json!({"kind":"binding","name":name,"args":args.iter().map(value_codec::encode).collect::<Vec<_>>()});
                let bytes = host::call(host_key, &request).map_err(host_error)?;
                let value = codec::parse(&bytes).map_err(host_error)?;
                value_codec::values(&value).map_err(host_error)
            });
            if result.functions.insert(path, function).is_some() {
                return Err(invalid("duplicate binding path"));
            }
        }
    }
    result.validate(environment(request)?).map_err(convert)?;
    Ok(result)
}
fn require_loader(
    request: &Value,
    host_key: u32,
) -> Result<Option<storm_lua_vm::source::RequireLoader>, BridgeError> {
    match request.get("requireLoader") {
        None | Some(Value::Bool(false)) => Ok(None),
        Some(Value::Bool(true)) => {
            if host_key == 0 {
                return Err(invalid("missing source provider"));
            }
            Ok(Some(storm_lua_vm::source::RequireLoader::new(
                move |name| {
                    let bytes = host::call(host_key, &json!({"kind":"source", "name":name}))
                        .map_err(host_error)?;
                    let result = codec::parse(&bytes).map_err(host_error)?;
                    let name = result["name"]
                        .as_str()
                        .ok_or_else(|| host_error(invalid("source chunk name is missing")))?
                        .to_owned();
                    let source = codec::byte_array(&result["source"]).map_err(host_error)?;
                    let chunk = storm_lua_vm::source::SourceChunk { name, source };
                    chunk.validate()?;
                    Ok(chunk)
                },
            )))
        }
        _ => Err(invalid("requireLoader must be Boolean")),
    }
}
pub(crate) fn create_vehicle(
    instructions: u32,
    memory: u32,
    host_key: u32,
    bytes: &[u8],
) -> Result<usize, BridgeError> {
    let request = codec::parse(bytes)?;
    let properties = match request.get("properties") {
        None => Default::default(),
        Some(value) => {
            codec::properties(&serde_json::to_vec(value).map_err(|e| invalid(&e.to_string()))?)?
        }
    };
    let vm = storm_lua_microcontroller::Microcontroller::new(
        storm_lua_microcontroller::MicrocontrollerConfig {
            limits: session::limits(instructions, memory)?,
            properties,
            environment: environment(&request)?,
            bindings: bindings(&request, host_key)?,
            require_loader: require_loader(&request, host_key)?,
        },
    )
    .map_err(convert)?;
    session::insert(Script::Vehicle(Box::new(vm)))
}
pub(crate) fn create_addon(
    instructions: u32,
    memory: u32,
    host_key: u32,
    bytes: &[u8],
) -> Result<usize, BridgeError> {
    let request = codec::parse(bytes)?;
    let entries = request["server"]
        .as_array()
        .ok_or_else(|| invalid("server must contain explicitly provided names"))?;
    if entries.len() > 512 || (!entries.is_empty() && host_key == 0) {
        return Err(invalid("invalid server provider configuration"));
    }
    let mut server = BTreeMap::new();
    for entry in entries {
        let name = entry
            .as_str()
            .ok_or_else(|| invalid("server function name must be text"))?
            .to_owned();
        let method = name.clone();
        let function: HostFunction = Rc::new(move |args| {
            let request = json!({"kind":"server","name":method,"args":args.iter().map(value_codec::encode).collect::<Vec<_>>()});
            let bytes = host::call(host_key, &request).map_err(host_error)?;
            let value = codec::parse(&bytes).map_err(host_error)?;
            value_codec::values(&value).map_err(host_error)
        });
        if server.insert(name, function).is_some() {
            return Err(invalid("duplicate server binding name"));
        }
    }
    let savedata = if request["savedata"].is_null() {
        None
    } else {
        Some(value_codec::value(&request["savedata"])?)
    };
    let property_bytes =
        serde_json::to_vec(&request["properties"]).map_err(|e| invalid(&e.to_string()))?;
    let config = AddonConfig {
        limits: session::limits(instructions, memory)?,
        is_world_create: request["newWorld"]
            .as_bool()
            .ok_or_else(|| invalid("newWorld must be Boolean"))?,
        properties: codec::properties(&property_bytes)?,
        savedata,
        server,
        dev_logs: false,
        environment: environment(&request)?,
        bindings: bindings(&request, host_key)?,
        require_loader: require_loader(&request, host_key)?,
    };
    session::insert(Script::Addon(Box::new(
        Addon::new(config).map_err(convert)?,
    )))
}
pub(crate) fn addon(handle: u32, bytes: &[u8]) -> Result<Status, BridgeError> {
    let request = codec::parse(bytes)?;
    session::with(handle, |session| {
        let vm = session.vm.addon()?;
        let mut status = Status::Ok;
        let response=match request["action"].as_str() {
            Some("start")=>{status=outcome(vm.start().map_err(convert)?);Value::Null},
            Some("tick")=>{status=outcome(vm.tick(number(&request["gameTicks"])?).map_err(convert)?);Value::Null},
            Some("dispatch")=>{
                let callback=request["callback"].as_str().ok_or_else(|| invalid("callback name is missing"))?;
                status=outcome(vm.dispatch(callback,&value_codec::values(&request["arguments"])?).map_err(convert)?);Value::Null
            },
            Some("destroy")=>{status=outcome(vm.destroy().map_err(convert)?);Value::Null},
            Some("savedata")=>value_codec::encode(&vm.savedata().map_err(convert)?),
            Some("reload")=>{status=outcome(vm.reload(value_codec::value(&request["savedata"])?).map_err(convert)?);Value::Null},
            Some("properties")=>Value::Array(vm.property_definitions().into_iter().map(|(label,definition)| match definition {
                MenuProperty::Checkbox(default)=>json!({"kind":"checkbox","label":label,"default":default}),
                MenuProperty::Slider([min,max,increment,default])=>json!({"kind":"slider","label":label,"min":min,"max":max,"increment":increment,"default":default}),
            }).collect()),
            _=>return Err(invalid("unknown addon operation")),
        };
        codec::respond(&response)?;
        Ok(status)
    })
}
fn token(value: &Value) -> Result<HttpToken, BridgeError> {
    Ok(HttpToken {
        generation: decimal(&value["generation"])?,
        id: number(&value["id"])?,
    })
}
pub(crate) fn http(handle: u32, bytes: &[u8]) -> Result<Status, BridgeError> {
    let request = codec::parse(bytes)?;
    session::with(handle, |session| {
        let mut status = Status::Ok;
        let response=match request["action"].as_str() {
            Some("drain")=>Value::Array(session.vm.drain_http_requests().into_iter().map(|r| json!({"token":{"generation":r.token.generation.to_string(),"id":r.token.id},"port":r.port,"request":r.request})).collect()),
            Some("reply")=>{
                let reply=codec::byte_array(&request["reply"])?;
                let result=session.vm.http_reply(token(&request["token"])?,&reply);
                session.sync_output();status=outcome(result.map_err(convert)?);Value::Null
            },
            Some("cancel")=>{session.vm.cancel_http(token(&request["token"])?).map_err(convert)?;Value::Null},
            _=>return Err(invalid("unknown HTTP operation")),
        };
        codec::respond(&response)?;
        Ok(status)
    })
}
pub(crate) fn logs(handle: u32, structured: bool) -> Result<Status, BridgeError> {
    session::with(handle, |session| {
        let logs = session.vm.drain_log_records();
        let records: Vec<_> = logs.into_iter().map(|record| {
            if structured {
                let location = record.location.map(|location| json!({"chunk": location.chunk, "line": location.line}));
                json!({"source": match record.source {LogSource::Print => "print", LogSource::Debug => "debug.log"}, "bytes": record.bytes, "location": location})
            } else {
                json!(record.bytes)
            }
        }).collect();
        codec::respond(&Value::Array(records))?;
        Ok(Status::Ok)
    })
}
