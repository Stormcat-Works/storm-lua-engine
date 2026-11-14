//! コールドパスのホストデバッガプロトコル。UIやトランスポートスケジューラは持ちません。
use crate::{
    codec,
    session::{self, convert, Session},
};
use serde_json::{json, Value};
use storm_lua_bridge::{BridgeError, Status};
use storm_lua_vm::{debug::DebugHandle, runner::StepMode};
fn invalid() -> BridgeError {
    BridgeError::new(Status::InvalidArgument, "malformed debugger request")
}
fn number(value: &Value) -> Result<u32, BridgeError> {
    value
        .as_u64()
        .and_then(|n| u32::try_from(n).ok())
        .ok_or_else(invalid)
}
fn decimal(value: &Value) -> Result<u64, BridgeError> {
    value
        .as_str()
        .ok_or_else(invalid)?
        .parse()
        .map_err(|_| invalid())
}
fn variables(values: Vec<storm_lua_vm::debug::Variable>) -> Value {
    Value::Array(
        values
            .into_iter()
            .map(|value| json!({"name":value.name,"value":codec::debug_value(value.value)}))
            .collect(),
    )
}
pub(crate) fn request(handle: u32, bytes: &[u8]) -> Result<Status, BridgeError> {
    let request = codec::parse(bytes)?;
    session::with(handle, |session| execute(session, &request))
}
fn execute(session: &mut Session, request: &Value) -> Result<Status, BridgeError> {
    let mut status = Status::Ok;
    let response=match request["action"].as_str(){
        Some("breakpoints")=>{let entries=request["points"].as_array().ok_or_else(invalid)?;
            if entries.len()>4096{return Err(invalid());}
            let points=entries.iter().map(|v|Ok((v["source"].as_str().ok_or_else(invalid)?.to_owned(),i32::try_from(number(&v["line"])?).map_err(|_|invalid())?))).collect::<Result<Vec<_>,BridgeError>>()?;
            session.vm.set_breakpoints(points).map_err(convert)?;json!({"ok":true})},
        Some("resume")=>{let mode=match request["mode"].as_str(){Some("continue")=>StepMode::Continue,Some("into")=>StepMode::Into,Some("over")=>StepMode::Over,Some("out")=>StepMode::Out,_=>return Err(invalid())};
            // carryは、ステップが入口の終わりを越えたときに次の入口の最初の行で止める指定。
            let carry=match request.get("carry"){None=>false,Some(Value::Bool(carry))=>*carry,_=>return Err(invalid())};
            status=session.resume(mode,carry)?;json!({"status":status as i32})},
        Some("requestPause")=>{session.vm.request_pause().map_err(convert)?;json!({"ok":true})},
        Some("cancelPause")=>{session.vm.cancel_pause();json!({"ok":true})},
        Some("stack")=>Value::Array(session.vm.stack().map_err(convert)?.into_iter().map(|frame|json!({"level":frame.level,"source":frame.source,"line":frame.line,"functionName":frame.function_name})).collect()),
        Some("locals")=>variables(session.vm.locals(number(&request["level"])?).map_err(convert)?),
        Some("upvalues")=>variables(session.vm.upvalues(number(&request["level"])?).map_err(convert)?),
        Some("watch")=>{
            // 式の評価によってscreen/outputバインディングが呼び出される可能性があります。
            // フレームのリースを無効化し、停止中のコールバックの既存コマンドを保持しつつ、新たに追加されたプレフィックスのみをフラッシュします。
            session.advance_epoch()?;
            let result=session.vm.evaluate_watch(number(&request["level"])? ,request["expression"].as_str().ok_or_else(invalid)?);
            session.sync_output();let replay=session.refresh_frame();
            let value=result.map_err(convert)?;replay?;codec::debug_value(value)
        },
        Some("table")=>{let h=&request["handle"];let handle=DebugHandle{vm_id:decimal(&h["vmId"])?,pause_epoch:decimal(&h["pauseEpoch"])?,slot:number(&h["slot"])?};
            Value::Array(session.vm.expand_table(handle,number(&request["start"])? as usize,number(&request["limit"])? as usize).map_err(convert)?.into_iter().map(|entry|json!({"key":codec::debug_value(entry.key),"value":codec::debug_value(entry.value)})).collect())},
        _=>return Err(invalid()),
    };
    codec::respond(&response)?;
    Ok(status)
}
