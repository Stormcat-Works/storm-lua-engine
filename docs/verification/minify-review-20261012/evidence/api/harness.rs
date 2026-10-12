use std::io::{self, BufRead};
use storm_lua_minify::passes;
use storm_lua_syntax::{parser::parse_source,print::Printer};
fn eval(code:&str,setup:&str,after:&str)->String{
 let lua=mlua::Lua::new();
 match lua.load(format!("{setup}\n{code}\n{after}")).eval::<mlua::Value>() {
   Ok(v)=>format!("{v:?}"),Err(e)=>format!("ERROR {e}")
 }
}
fn main(){for line in io::stdin().lock().lines(){
 let line=line.unwrap();let v:serde_json::Value=serde_json::from_str(&line).unwrap();let source=v["source"].as_str().unwrap();let pass=v["pass"].as_str().unwrap_or("compile");let now=std::time::Instant::now();let mut validation=serde_json::Value::Null;
 let (code,metadata)=if pass=="compile" {let opt=serde_json::from_value(v.get("options").cloned().unwrap_or(serde_json::json!({}))).unwrap();let result=storm_lua_build::public_api::compile(source,&opt);let out=result.code.clone().unwrap_or_else(||format!("ERROR {:?}",result.error));if let Some(map)=&result.map{validation=serde_json::json!(storm_lua_build::optimized_source_map::validate(&out,map).map(|_|"valid"));}(out,serde_json::to_value(result).unwrap())} else {
 let (mut ast,root)=parse_source(source).unwrap();let r=match pass {
 "buttons"=>passes::screen_buttons::outline_screen_buttons(&mut ast,root,true),
 "slots"=>passes::callback_slots::slot_callback_exclusive_functions(&mut ast,root),
 "aliases"=>passes::api_aliases::optimize_api_aliases(&mut ast,root),
 "colors"=>passes::screen_call_factoring::factor_repeated_screen_calls(&mut ast,root),
 "output"=>passes::output_loops::synthesize_output_loops(&mut ast,root),
 "screen"=>passes::screen_loops::synthesize_screen_loops(&mut ast,root,false),
 "screen-periodic"=>passes::screen_loops::synthesize_screen_loops(&mut ast,root,true),
 "records"=>passes::draw_records::pack_draw_records(&mut ast,root,true),
 "motifs"=>passes::draw_sequences::outline_draw_sequences(&mut ast,root,true),
 _=>panic!("unknown pass")};(Printer::new(&ast,false).output(r.root),serde_json::json!({"saved":r.saved,"details":r.details}))};
 let elapsed=now.elapsed().as_secs_f64();let setup=v["setup"].as_str().unwrap_or("");let after=v["after"].as_str().unwrap_or("");let (original,output)=if v["skip_eval"].as_bool().unwrap_or(false){("skipped".into(),"skipped".into())}else{(eval(source,setup,after),eval(&code,setup,after))};
 println!("{}",serde_json::json!({"name":v["name"],"pass":pass,"source":source,"code":code,"original":original,"output":output,"elapsed_s":elapsed,"metadata":metadata,"map_validation":validation}));
}}
