//! アドオン専用の名前空間の組み込みおよび新規ワールドのプロパティ宣言。
use super::{MenuProperty, State};
use std::{cell::RefCell, rc::Rc};
use storm_lua_spec::property::PropertyValue;
use storm_lua_vm::backend::{BackendError, BackendResult, Lua, LuaString, Table, Value};
fn invalid(message: &str) -> BackendError {
    BackendError::external(super::invalid(message))
}
fn declare(state: &mut State, name: Vec<u8>, definition: MenuProperty) {
    // 実ゲームの契約: 宣言は名前の長さ・件数・既存宣言との矛盾でエラーを投げません。
    // 同名の再宣言は最後の宣言が有効です。
    state.definitions.insert(name, definition);
}
pub(super) fn install(lua: &Lua, env: &Table, state: Rc<RefCell<State>>) -> BackendResult<()> {
    let property = lua.create_table()?;
    let s = Rc::clone(&state);
    property.raw_set(
        "checkbox",
        lua.create_function(move |_, (label, default): (LuaString, bool)| {
            let name = label.as_bytes().to_vec();
            let mut state = s.borrow_mut();
            declare(&mut state, name.clone(), MenuProperty::Checkbox(default));
            if !state.new_world {
                return Ok(Value::Nil);
            }
            match state.properties.get(&name) {
                Some(PropertyValue::Bool(value)) => Ok(Value::Boolean(*value)),
                None => Ok(Value::Boolean(default)),
                _ => Err(invalid("checkbox override must be Boolean")),
            }
        })?,
    )?;
    let s = Rc::clone(&state);
    property.raw_set(
        "slider",
        lua.create_function(
            move |_, (label, min, max, increment, default): (LuaString, f64, f64, f64, f64)| {
                // 実ゲームの契約: 範囲・刻み・既定値の形は検証せず、宣言された値をそのまま保持します。
                let name = label.as_bytes().to_vec();
                let mut state = s.borrow_mut();
                declare(
                    &mut state,
                    name.clone(),
                    MenuProperty::Slider([min, max, increment, default]),
                );
                if !state.new_world {
                    return Ok(Value::Nil);
                }
                match state.properties.get(&name) {
                    Some(PropertyValue::Number(value))
                        if value.is_finite() && *value >= min && *value <= max =>
                    {
                        Ok(Value::Number(*value))
                    }
                    None => Ok(Value::Number(default)),
                    _ => Err(invalid(
                        "slider override must be a finite number within its range",
                    )),
                }
            },
        )?,
    )?;
    let server = lua.create_table()?;
    server.raw_set(
        "httpGet",
        lua.create_function(move |_, (port, request): (i64, LuaString)| {
            state
                .borrow_mut()
                .http
                .request(port, request.as_bytes().as_ref())
                .map_err(|e| BackendError::external(super::http_error(e)))
        })?,
    )?;
    env.raw_set("server", server)?;
    env.raw_set("property", property)?;
    super::matrix::install(lua, env)?;
    Ok(())
}
