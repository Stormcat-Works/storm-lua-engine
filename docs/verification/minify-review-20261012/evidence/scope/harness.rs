use std::io::{self,BufRead};
use mlua::{Lua,Value};
use storm_lua_minify::{compile_code,CompileOptions,CompileMode,NumericMode,passes,scope_rename};
use storm_lua_syntax::{parser::parse_source,print::Printer};
fn execute(code:&str,ticks:usize)->String {
 let lua=Lua::new(); let budget=std::cell::Cell::new(0); lua.set_hook(mlua::HookTriggers::new().every_nth_instruction(10000),move |_,_|{budget.set(budget.get()+1);if budget.get()>100 {Err(mlua::Error::RuntimeError("AUDIT instruction budget".into()))}else{Ok(mlua::VmState::Continue)}});
 let setup=r#"observed={} function record(...) local s={} for i=1,select('#',...)do local v=select(i,...) s[#s+1]=type(v)..':'..tostring(v) end observed[#observed+1]=table.concat(s,',') end output={setNumber=record,setBool=record} input={getNumber=function(i)return i*3 end,getBool=function(i)return i%2==0 end} property={getNumber=function(i)return 3 end,getBool=function(i)return true end,getText=function(i)return 'value' end} screen={drawText=record,drawLine=record,setColor=record,drawClear=record,getWidth=function()return 32 end,getHeight=function()return 32 end}"#;
 if let Err(e)=lua.load(setup).exec(){return format!("setup-error:{e}")}
 if let Err(e)=lua.load(code).exec(){return format!("load-error:{e}")}
 for _ in 0..ticks {
  if let Ok(f)=lua.globals().get::<mlua::Function>("onTick") {if let Err(e)=f.call::<()>(()){return format!("tick-error:{e}")}}
  if let Ok(f)=lua.globals().get::<mlua::Function>("onDraw") {if let Err(e)=f.call::<()>(()){return format!("draw-error:{e}")}}
 }
 match lua.load("return table.concat(observed,'|')").eval::<Value>() {Ok(Value::String(s))=>s.to_string_lossy(),Ok(v)=>format!("{v:?}"),Err(e)=>format!("result-error:{e}")}
}
fn run(v:serde_json::Value)->serde_json::Value {
 let source=v["source"].as_str().unwrap(); let mode=v["mode"].as_str().unwrap_or("default"); let ticks=v["ticks"].as_u64().unwrap_or(2) as usize;
 let mut passes_done=vec![]; let started=std::time::Instant::now();
 let output:Result<String,String>=if ["default","exact","safe"].contains(&mode){
  let mut opts=CompileOptions::default();opts.zero_cost_newlines=v["zero_cost_newlines"].as_bool().unwrap_or(true);
  if let Some(only)=v["only"].as_array(){for id in storm_lua_minify::pass_ids::OPTIMIZATION_PASS_IDS{opts.pass_toggles.insert(id,only.iter().any(|v|v.as_str()==Some(*id)));}}
  if mode=="exact" {opts.numeric_mode=NumericMode::Exact} if mode=="safe" {opts.mode=CompileMode::Safe;opts.numeric_mode=NumericMode::Exact}
  if let Some(t)=v["toggles"].as_object(){for (k,b) in t {opts.pass_toggles.insert(Box::leak(k.clone().into_boxed_str()),b.as_bool().unwrap());}}
  compile_code(source,&opts).map(|r| {passes_done=r.passes.iter().map(|p|p.name.clone()).collect(); r.code})
 }else{parse_source(source).map_err(|e|e.to_string()).map(|(mut ast,root)|{
  let mut r=root;
  match mode {
   "rename"=>{let o=scope_rename::scope_rename(&ast,root);ast=o.ast;r=o.root;},
   "final"=>{r=passes::final_stores::eliminate_overwritten_assignments(&mut ast,root).root;},
   "root"=>{r=passes::root_globals::globalize_root_locals(&mut ast,root).root;},
   "function"=>{r=passes::function_globalization::globalize_function_locals(&mut ast,root,0).root;},
   "locals"=>{r=passes::locals::inline_single_use_locals(&mut ast,root).root;},
   "forward"=>{r=passes::single_use_forwarding::forward_single_use_globals(&mut ast,root).root;},
   "closed-fields"=>{r=passes::closed_fields::rename_closed_fields(&mut ast,root).root;},
   "global-cleanup"=>{r=passes::global_stores::cleanup_global_stores(&mut ast,root).root;},
   "immutable-synth"=>{r=passes::immutable_synthesis::synthesize_immutable_carriers(&mut ast,root).root;},
   "immutable-reuse"=>{r=passes::immutable_values::reuse_immutable_values(&mut ast,root).root;},
   "destructive"=>{r=passes::destructive_results::globalize_destructive_results(&mut ast,root).root;},
   "captured"=>{r=passes::captured_use::hoist_captured_single_uses(&mut ast,root).root;},
   "table-param"=>{r=passes::tables::scalarize_table_literal_parameters(&mut ast,root,16).root;},
   "namespace"=>{r=passes::tables::scalarize_closed_namespaces(&mut ast,root,8).root;},
   "immutable-table"=>{r=passes::tables::flatten_immutable_tables(&mut ast,root,8).root;},
   "adjacent"=>{r=passes::adjacent_locals::pack_adjacent_locals(&mut ast,root,false).root;},
   _=>{}
  };Printer::new(&ast,false).output(r)
 })};
 let transform_us=started.elapsed().as_micros(); if v["bench"].as_bool()==Some(true){return serde_json::json!({"transform_us":transform_us,"output_bytes":output.as_ref().map(|s|s.len()).unwrap_or(0),"mode":mode})}
 match output {Ok(code)=>serde_json::json!({"source":source,"mode":mode,"code":code,"before":execute(source,ticks),"after":execute(&code,ticks),"passes":passes_done,"transform_us":transform_us}),Err(e)=>serde_json::json!({"source":source,"mode":mode,"error":e})}
}
fn main(){for line in io::stdin().lock().lines(){let s=line.unwrap(); let result=std::panic::catch_unwind(|| run(serde_json::from_str(&s).unwrap())); match result{Ok(v)=>println!("{}",v),Err(_)=>println!("{{\"panic\":true}}")}}}
