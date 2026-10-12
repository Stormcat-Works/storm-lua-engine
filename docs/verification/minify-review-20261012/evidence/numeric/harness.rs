use std::io::{self, BufRead};
use storm_lua_minify::{compile_code,CompileOptions,CompileMode,NumericMode,SearchMode};
fn eval(src:&str, req:&serde_json::Value)->String{
 let lua=mlua::Lua::new();
 if let Some(setup)=req["setup"].as_str(){lua.load(setup).exec().unwrap();}
 let value=if let Some(run)=req["run"].as_str(){lua.load(src).exec().and_then(|_|lua.load(run).eval::<mlua::MultiValue>())}else{lua.load(src).eval::<mlua::MultiValue>()};
 match value {
  Ok(v)=>format!("{:?}",v),
  Err(e)=>format!("ERROR: {}", e.to_string().lines().take(2).collect::<Vec<_>>().join(" ")),
 }
}
fn main(){
 std::panic::set_hook(Box::new(|_| {}));
 for line in io::stdin().lock().lines(){
  let line=line.unwrap(); if line.trim().is_empty(){continue}
  let req:serde_json::Value=serde_json::from_str(&line).unwrap();
  let src=req["source"].as_str().unwrap();
  let mode=req["mode"].as_str().unwrap_or("default");
  let mut opt=CompileOptions::default();
  if mode.contains("safe"){opt.mode=CompileMode::Safe;}
  if mode.contains("exact"){opt.numeric_mode=NumericMode::Exact;}
  if req["fast"].as_bool().unwrap_or(false){opt.search_mode=SearchMode::Fast;}
  if let Some(n)=req["target"].as_u64(){opt.target_size=Some(n as usize);}
  if req["origin"].as_bool().unwrap_or(false){opt.origin_source=Some("audit.lua".into());}
  if let Some(only)=req["only"].as_array(){
   for id in storm_lua_minify::pass_ids::OPTIMIZATION_PASS_IDS {opt.pass_toggles.insert(id,false);}
   for id in only{let id=id.as_str().unwrap();let key=storm_lua_minify::pass_ids::OPTIMIZATION_PASS_IDS.iter().find(|x|**x==id).unwrap();opt.pass_toggles.insert(key,true);}
  }
  let original=eval(src,&req);
  let start=std::time::Instant::now();
  let compiled=std::panic::catch_unwind(||compile_code(src,&opt));
  match compiled {
   Ok(Ok(out))=> {let actual=eval(&out.code,&req);println!("{}",serde_json::json!({"request":req,"source":src,"mode":mode,"only":req["only"],"target":req["target"],"origin":req["origin"],"fast":req["fast"],"expected":original,"actual":actual,"same":original==actual,"output":out.code,"passes":out.passes,"elapsed_ms":start.elapsed().as_secs_f64()*1000.0}));},
   Ok(Err(e))=>println!("{}",serde_json::json!({"request":req,"source":src,"mode":mode,"expected":original,"compile_error":e})),
   Err(p)=>println!("{}",serde_json::json!({"request":req,"source":src,"mode":mode,"expected":original,"panic":p.downcast_ref::<String>().cloned().or_else(||p.downcast_ref::<&str>().map(|s|s.to_string())).unwrap_or_else(||"unknown panic".into())})),
  }
 }
}
