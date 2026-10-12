
use std::io::{self, BufRead};
use serde_json::{json,Value};
use mlua::{Lua,MultiValue};
fn evaluate(s:&str)->Value {
 let lua=Lua::new();
 let count=std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
 let count2=count.clone();
 lua.set_hook(mlua::HookTriggers::new().every_nth_instruction(10000),move |_,_| {
  if count2.fetch_add(1,std::sync::atomic::Ordering::Relaxed)>100 { Err(mlua::Error::RuntimeError("audit instruction budget exceeded".into())) }
  else { Ok(mlua::VmState::Continue) }
 });
 match lua.load(s).set_name("=audit").eval::<MultiValue>() {
  Ok(vals)=>json!({"ok":true,"values":vals.into_iter().map(|v| match v {
   mlua::Value::Nil=>json!({"nil":true}),
   mlua::Value::Boolean(b)=>json!({"bool":b}),
   mlua::Value::Integer(i)=>json!({"int":i.to_string()}),
   mlua::Value::Number(f)=>json!({"floatBits":format!("{:016x}",f.to_bits()),"repr":f.to_string()}),
   mlua::Value::String(s)=>json!({"bytes":s.as_bytes().iter().map(|b|format!("{b:02x}")).collect::<String>()}),
   _=>json!({"type":v.type_name()})
  }).collect::<Vec<_>>()}),
  Err(e)=>json!({"ok":false,"error":e.to_string()})
 }
}
fn process(v:Value)->Value {
 let src=v["source"].as_str().unwrap();
 let action=v["action"].as_str().unwrap_or("minify");
 if action=="eval" {return evaluate(src)};
 if action=="print" || action=="token" {
  let result=if action=="token" {storm_lua_syntax::print::token_minify(src).map_err(|e|e.to_string())}
  else {storm_lua_syntax::parser::parse_source(src).map(|(a,r)|storm_lua_syntax::print::Printer::new(&a,false).output(r)).map_err(|e|e.to_string())};
  return match result {Ok(code)=>json!({"ok":true,"original":evaluate(src),"generated":evaluate(&code),"code":code}),Err(e)=>json!({"ok":false,"error":e,"original":evaluate(src)})}
 }
 let opts=serde_json::from_value::<storm_lua_build::ApiCompileOptions>(v.get("options").cloned().unwrap_or(json!({}))).unwrap();
 let now=std::time::Instant::now();
 let compiled=storm_lua_build::minify(src,&opts);
 let elapsed=now.elapsed().as_secs_f64();
 json!({"result":compiled,"seconds":elapsed,"original":evaluate(src),"generated":compiled.code.as_ref().map(|c|evaluate(c))})
}
fn main(){ for line in io::stdin().lock().lines(){let line=line.unwrap();let v=serde_json::from_str(&line).unwrap();let result=std::panic::catch_unwind(||process(v));println!("{}",result.unwrap_or(json!({"panic":true})));}}
