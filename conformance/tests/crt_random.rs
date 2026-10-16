//! Adopted independent Windows CRT/Lua primitive traces; no native bytes required.
#![allow(clippy::unwrap_used, clippy::panic)]
use serde_json::Value as Json;
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};
use storm_lua_vm::{
    backend::{MultiValue, Value},
    random::{CrtRandomSource, SharedCrtRandom},
    runner::Vm,
};
struct Tape {
    values: Vec<u32>,
    cursor: Cell<usize>,
    seeds: RefCell<Vec<u32>>,
}
impl CrtRandomSource for Tape {
    fn draw(&self) -> Result<u32, String> {
        let i = self.cursor.get();
        let value = *self.values.get(i).ok_or("native draw tape exhausted")?;
        self.cursor.set(i + 1);
        Ok(value)
    }
    fn seed(&self, value: u32) -> Result<(), String> {
        self.seeds.borrow_mut().push(value);
        Ok(())
    }
}
fn value(v: &Json) -> Value {
    match v["kind"].as_str().unwrap() {
        "integer" => Value::Integer(v["value"].as_i64().unwrap()),
        "number" => Value::Number(f64::from_bits(v["bits"].as_u64().unwrap())),
        "boolean" => Value::Boolean(v["value"].as_bool().unwrap()),
        "nil" => Value::Nil,
        kind => panic!("unsupported native fixture kind {kind}"),
    }
}
#[test]
fn original_windows_sampler_120_random_and_27_seed_cases() {
    let fixture: Json =
        serde_json::from_str(include_str!("../../fixtures/native-crt-random.json")).unwrap();
    assert_eq!(fixture["schema"], 1);
    assert_eq!(
        fixture["provenance"]["binary_sha256"],
        "b261e81eb8a4e5bb84782e521212dcc94151fc2c2b5940bcff5aabef85bbe20f"
    );
    assert_eq!(
        fixture["provenance"]["oracle_sha256"],
        "baa65bc311bb1137f290631e096185b9b2d2b62076e5a5e1525dc8d26f9da63e"
    );
    let mut counts = [0; 2];
    for case in fixture["cases"].as_array().unwrap() {
        let context = format!("{} {}", case["function"], case["name"]);
        let tape = Rc::new(Tape {
            values: serde_json::from_value(case["raw_draws"].clone()).unwrap(),
            cursor: Cell::new(0),
            seeds: RefCell::new(Vec::new()),
        });
        let mut vm = Vm::new(Default::default()).unwrap();
        let source: SharedCrtRandom = tape.clone();
        vm.install_crt_random(source).unwrap();
        let function = case["function"].as_str().unwrap();
        counts[usize::from(function == "randomseed")] += 1;
        let args: MultiValue = case["arguments"]
            .as_array()
            .unwrap()
            .iter()
            .map(value)
            .collect();
        let result = vm.configure(|_, env| {
            env.get::<storm_lua_vm::backend::Table>("math")?
                .get::<storm_lua_vm::backend::Function>(function)?
                .call::<MultiValue>(args)
        });
        assert_eq!(
            result.is_err(),
            case["error"].as_bool().unwrap(),
            "{context}: {result:?}"
        );
        assert_eq!(
            tape.cursor.get(),
            tape.values.len(),
            "{context} consumption"
        );
        let seeds: Vec<u32> = serde_json::from_value(case["seeds"].clone()).unwrap();
        assert_eq!(*tape.seeds.borrow(), seeds, "{context} seed words");
        if let Ok(values) = result {
            let expected = case["values"].as_array().unwrap();
            assert_eq!(values.len(), expected.len(), "{context} count");
            for (a, b) in values.iter().zip(expected) {
                match (a, value(b)) {
                    (Value::Integer(a), Value::Integer(b)) => assert_eq!(*a, b, "{context}"),
                    (Value::Number(a), Value::Number(b)) => {
                        assert_eq!(a.to_bits(), b.to_bits(), "{context}")
                    }
                    pair => panic!("{context}: return type/value differs {pair:?}"),
                }
            }
        }
    }
    assert_eq!(counts, [120, 27]);
}
