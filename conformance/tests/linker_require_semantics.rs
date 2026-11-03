//! Linker require-semantics regressions (Codex run5 Q06-Q08 and
//! run6 R06-R08): the linked output is executed under the real Lua
//! backend and compared against PUC `require` semantics.
use std::collections::BTreeMap;
use std::error::Error;
use storm_lua_analysis::LuaProject;
use storm_lua_build::link::link_project;
use storm_lua_vm::backend::{Lua, MultiValue, Value};

fn project(modules: &[(&str, &str)]) -> LuaProject {
    LuaProject {
        entry: "main".to_string(),
        modules: modules
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect::<BTreeMap<_, _>>(),
        ambient: BTreeMap::new(),
    }
}

fn linked_values(p: &LuaProject) -> Result<Vec<Value>, Box<dyn Error>> {
    let result = link_project(p);
    assert!(
        result.diagnostics.is_empty(),
        "link diagnostics: {:?}",
        result
            .diagnostics
            .iter()
            .map(|d| d.code)
            .collect::<Vec<_>>()
    );
    let src = result.linked_source.ok_or("missing linked source")?;
    let values: MultiValue = Lua::new().load(&src).eval()?;
    Ok(values.into_iter().collect())
}

#[test]
fn goto_skipped_first_require_still_initializes_module() -> Result<(), Box<dyn Error>> {
    // Q06: the initialization glue is emitted at every require site,
    // so a goto that skips the first site no longer leaves the second
    // site reading the uninitialized loader function as the value.
    let p = project(&[
        (
            "main",
            "goto L\nrequire(\"m\")\n::L::\nlocal n=require(\"m\")\nreturn n",
        ),
        ("m", "return 7"),
    ]);
    assert_eq!(linked_values(&p)?, vec![Value::Integer(7)]);
    Ok(())
}

#[test]
fn false_returning_module_is_reloaded_on_next_require() -> Result<(), Box<dyn Error>> {
    // Q07: like package.loaded truthiness, a module returning false
    // is loaded again by the next require; a module returning nothing
    // (nil) is normalized to true and loaded once.
    let p = project(&[
        (
            "main",
            "local a=require(\"m\")\nlocal b=require(\"m\")\nreturn a,b,n",
        ),
        ("m", "n=(n or 0)+1\nreturn false"),
    ]);
    assert_eq!(
        linked_values(&p)?,
        vec![
            Value::Boolean(false),
            Value::Boolean(false),
            Value::Integer(2)
        ]
    );
    let p = project(&[
        ("main", "require(\"m\")\nrequire(\"m\")\nreturn n"),
        ("m", "n=(n or 0)+1"),
    ]);
    assert_eq!(linked_values(&p)?, vec![Value::Integer(1)]);
    Ok(())
}

#[test]
fn require_glue_survives_local_env_shadowing() -> Result<(), Box<dyn Error>> {
    // Q08: generated references resolve through the lexical registry,
    // not the (shadowed) global environment.
    let p = project(&[
        (
            "main",
            "local _ENV={require=require}\nlocal m=require(\"m\")\nreturn m",
        ),
        ("m", "return 7"),
    ]);
    assert_eq!(linked_values(&p)?, vec![Value::Integer(7)]);
    Ok(())
}

#[test]
fn registry_stays_lexical_at_the_local_cap_with_env_shadowing() -> Result<(), Box<dyn Error>> {
    // R06: at 199 entry locals the registry cannot be a chunk local,
    // but the wrapped form keeps it lexical, so a `local _ENV` that
    // shadows the global environment cannot break the require glue
    // (the old global-registry fallback failed with "attempt to index
    // a nil value (global '__stormmin_link_modules')").
    let locals: Vec<String> = (0..199).map(|i| format!("p{i}")).collect();
    let main = format!(
        "local {}\nlocal _ENV={{require=require}}\nrequire(\"m\")\nreturn 7",
        locals.join(",")
    );
    let p = project(&[("main", main.as_str()), ("m", "return 9")]);
    assert_eq!(linked_values(&p)?, vec![Value::Integer(7)]);
    Ok(())
}

#[test]
fn modules_do_not_share_the_entry_chunks_mutable_env() -> Result<(), Box<dyn Error>> {
    // R07: the entry chunk rebinding its own `_ENV` must not leak into
    // modules. Each module is bound to the real global environment,
    // like a standard require (entry reads x as 2 from its rebound
    // env, the module still reads the global x = 1).
    let p = project(&[
        (
            "main",
            "x=1\n_ENV={require=require,x=2}\nlocal m=require(\"m\")\nreturn m",
        ),
        ("m", "return x"),
    ]);
    assert_eq!(linked_values(&p)?, vec![Value::Integer(1)]);
    Ok(())
}

#[test]
fn module_local_env_rebinding_does_not_leak_back() -> Result<(), Box<dyn Error>> {
    // R07 companion: a module rebinding its own `_ENV` affects only
    // itself; the entry chunk and sibling modules keep the global env.
    let p = project(&[
        (
            "main",
            "x=1\nlocal a=require(\"a\")\nlocal b=require(\"b\")\nreturn a,b,x",
        ),
        ("a", "_ENV={x=99}\nreturn x"),
        ("b", "return x"),
    ]);
    assert_eq!(
        linked_values(&p)?,
        vec![Value::Integer(99), Value::Integer(1), Value::Integer(1)]
    );
    Ok(())
}

#[test]
fn entry_at_the_register_cap_still_links_loadable_output() -> Result<(), Box<dyn Error>> {
    // R08: an entry whose frame is at the register cap (a 253-value
    // return) is loadable on its own, so the linked output must be
    // loadable too. The registry moves out of the entry frame in the
    // wrapped form instead of pushing the peak over the limit.
    let values: Vec<String> = (0..253).map(|i| i.to_string()).collect();
    let main = format!("local m=require(\"m\")\nreturn {}", values.join(","));
    let p = project(&[("main", main.as_str()), ("m", "return 7")]);
    let got = linked_values(&p)?;
    assert_eq!(got.len(), 253);
    assert_eq!(got[0], Value::Integer(0));
    assert_eq!(got[252], Value::Integer(252));
    Ok(())
}
