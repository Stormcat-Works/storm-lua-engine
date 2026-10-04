//! Stream the existing schema-1 canonical JSON without first expanding every
//! record into serde_json::Value. Fields are written in lexicographic key order,
//! exactly as the previous Value/BTreeMap encoder did. This is not a new format.
use crate::optimized_source_map::{MapDisposition, MapExtension, MapOrigin, MapProducer};
use serde::ser::{SerializeMap, SerializeSeq};
use serde::{Serialize, Serializer};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::io::{BufWriter, Write};
use storm_lua_syntax::explanation::{InlineContext, OptimizationReason, SourceRelation};
use storm_lua_syntax::provenance::{GeneratedConstruct, OriginMapping, SourceSpan};

struct Canonical<'a, T>(&'a T);
struct Items<'a, T>(&'a [T]);
impl<T> Serialize for Items<'_, T>
where
    for<'a> Canonical<'a, T>: Serialize,
{
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut seq = serializer.serialize_seq(Some(self.0.len()))?;
        for item in self.0 {
            seq.serialize_element(&Canonical(item))?;
        }
        seq.end()
    }
}

impl Serialize for Canonical<'_, SourceSpan> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let v = self.0;
        let mut m = s.serialize_map(Some(3))?;
        m.serialize_entry("end", &v.end)?;
        m.serialize_entry("source", &v.source)?;
        m.serialize_entry("start", &v.start)?;
        m.end()
    }
}
impl Serialize for Canonical<'_, SourceRelation> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let v = self.0;
        let mut m = s.serialize_map(Some(2))?;
        m.serialize_entry("role", &v.role)?;
        m.serialize_entry("span", &Canonical(&v.span))?;
        m.end()
    }
}
impl Serialize for Canonical<'_, InlineContext> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let v = self.0;
        let mut m = s.serialize_map(Some(2))?;
        m.serialize_entry("callSite", &Canonical(&v.call_site))?;
        m.serialize_entry("definition", &Canonical(&v.definition))?;
        m.end()
    }
}
impl Serialize for Canonical<'_, OptimizationReason> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let v = self.0;
        let mut m = s.serialize_map(Some(6))?;
        m.serialize_entry("after", &v.after)?;
        m.serialize_entry("basis", &v.basis)?;
        m.serialize_entry("before", &v.before)?;
        m.serialize_entry("code", &v.code)?;
        m.serialize_entry("facts", &v.facts)?;
        m.serialize_entry("operation", &v.operation)?;
        m.end()
    }
}
impl Serialize for Canonical<'_, MapOrigin> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let v = self.0;
        let mut m = s.serialize_map(Some(7))?;
        m.serialize_entry("kind", &v.kind)?;
        m.serialize_entry("name", &v.name)?;
        m.serialize_entry("precision", &v.precision)?;
        m.serialize_entry("primary", &v.primary.as_ref().map(Canonical))?;
        m.serialize_entry("reasons", &v.reasons)?;
        m.serialize_entry("related", &v.related)?;
        m.serialize_entry("transformation", &v.transformation)?;
        m.end()
    }
}
impl Serialize for Canonical<'_, OriginMapping> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let v = self.0;
        let mut m = s.serialize_map(Some(5))?;
        m.serialize_entry("copied", &v.copied.as_ref().map(Canonical))?;
        m.serialize_entry("end", &v.end)?;
        m.serialize_entry("inlineContexts", &v.inline_contexts)?;
        m.serialize_entry("origin", &v.origin)?;
        m.serialize_entry("start", &v.start)?;
        m.end()
    }
}
impl Serialize for Canonical<'_, GeneratedConstruct> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let v = self.0;
        let mut m = s.serialize_map(Some(3))?;
        m.serialize_entry("end", &v.end)?;
        m.serialize_entry("origin", &v.origin)?;
        m.serialize_entry("start", &v.start)?;
        m.end()
    }
}
impl Serialize for Canonical<'_, MapDisposition> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let v = self.0;
        let mut m = s.serialize_map(Some(3))?;
        m.serialize_entry("original", &Canonical(&v.original))?;
        m.serialize_entry("reason", &v.reason)?;
        m.serialize_entry("replacementSources", &Items(&v.replacement_sources))?;
        m.end()
    }
}
impl Serialize for Canonical<'_, MapProducer> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let v = self.0;
        let mut m = s.serialize_map(Some(4))?;
        m.serialize_entry("dirty", &v.dirty)?;
        m.serialize_entry("name", &v.name)?;
        m.serialize_entry("revision", &v.revision)?;
        m.serialize_entry("version", &v.version)?;
        m.end()
    }
}
impl Serialize for Canonical<'_, MapExtension> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let v = self.0;
        let mut m = s.serialize_map(Some(14))?;
        m.serialize_entry("compilation", &v.compilation)?;
        m.serialize_entry("constructs", &Items(&v.constructs))?;
        m.serialize_entry("contexts", &Items(&v.contexts))?;
        m.serialize_entry("coordinateUnit", &v.coordinate_unit)?;
        m.serialize_entry("dispositions", &Items(&v.dispositions))?;
        m.serialize_entry("generated", &v.generated)?;
        m.serialize_entry("integrity", &v.integrity)?;
        m.serialize_entry("mappings", &Items(&v.mappings))?;
        m.serialize_entry("origins", &Items(&v.origins))?;
        m.serialize_entry("producer", &Canonical(&v.producer))?;
        m.serialize_entry("reasons", &Items(&v.reasons))?;
        m.serialize_entry("relations", &Items(&v.relations))?;
        m.serialize_entry("schemaVersion", &v.schema_version)?;
        m.serialize_entry("sources", &v.sources)?;
        m.end()
    }
}
struct Document<'a> {
    standard: &'a serde_json::Map<String, Value>,
    extension: &'a MapExtension,
}
impl Serialize for Document<'_> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut m = s.serialize_map(Some(self.standard.len() + 1))?;
        // The standard producer emits fields preceding x_storm. Sorting all keys
        // through a Value was the old encoder's definition of the fingerprint.
        for (name, value) in self.standard {
            m.serialize_entry(name, value)?;
        }
        m.serialize_entry("x_storm", &Canonical(self.extension))?;
        m.end()
    }
}
struct HashWriter<'a>(&'a mut Sha256);
impl Write for HashWriter<'_> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.update(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

pub(crate) fn encode(standard: &Value, mut extension: MapExtension) -> Result<String, String> {
    let standard = standard
        .as_object()
        .ok_or("standard source map is not an object")?;
    let mut hash = Sha256::new();
    {
        let mut writer = BufWriter::with_capacity(16 * 1024, HashWriter(&mut hash));
        serde_json::to_writer(
            &mut writer,
            &Document {
                standard,
                extension: &extension,
            },
        )
        .map_err(|e| e.to_string())?;
        writer.flush().map_err(|e| e.to_string())?;
    }
    extension.integrity = format!("{:x}", hash.finalize());
    serde_json::to_string(&Document {
        standard,
        extension: &extension,
    })
    .map_err(|e| e.to_string())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod canonical_tests {
    use super::*;
    use crate::optimized_source_map::validate;
    use crate::public_api::{
        compile, compile_project, ApiCompileOptions, ApiProjectCompileOptions,
    };
    use storm_lua_analysis::project::LuaProject;

    // Independent reference of the released schema-1 materialized encoder. It
    // hashes sorted JSON with an empty integrity field, then writes it again.
    fn old_encode(standard: &Value, mut extension: MapExtension) -> String {
        extension.integrity.clear();
        let mut value = standard.clone();
        value["x_storm"] = serde_json::to_value(extension).unwrap();
        let hash = format!("{:x}", Sha256::digest(serde_json::to_vec(&value).unwrap()));
        value["x_storm"]["integrity"] = Value::String(hash);
        serde_json::to_string(&value).unwrap()
    }
    fn check(code: &str, map: &str) {
        let mut original: Value = serde_json::from_str(map).unwrap();
        let mut extension = validate(code, map).unwrap();
        extension.integrity.clear();
        original.as_object_mut().unwrap().remove("x_storm");
        assert!(original
            .as_object()
            .unwrap()
            .keys()
            .all(|k| k.as_str() < "x_storm"));
        // Also detects new Rust fields accidentally omitted by the private writer.
        assert_eq!(
            serde_json::to_value(Canonical(&extension)).unwrap(),
            serde_json::to_value(&extension).unwrap()
        );
        let expected = old_encode(&original, extension.clone());
        let actual = encode(&original, extension).unwrap();
        assert_eq!(
            actual, expected,
            "exact canonical bytes and fingerprint differ"
        );
        assert_eq!(actual, map, "regenerated map differs");
        validate(code, &actual).unwrap();
    }
    #[test]
    fn streamed_map_matches_value_encoding_including_unicode_reasons_and_identity() {
        for text in [
            "local function twice(x)return x*2 end function onTick()local unused=99 output.setNumber(1,twice(input.getNumber(1))+2*3)end",
            "-- 雪😀\r\nfunction onTick()output.setNumber(1,property.getNumber('速度'))end\r\n",
            "local function f(a,b)return a+b end x=f(1,200)y=f(3,200)",
            "local unused=99 if false then output.setNumber(1,1)end",
            "local _ENV=_ENV -- same lexical contract\nfunction onTick()output.setNumber(1,2)end",
            "",
        ] {
            for target in [None,Some(8192usize)] {
                let options:ApiCompileOptions=serde_json::from_value(serde_json::json!({"sourceMap":true,"sourceName":"controller.lua","targetSize":target})).unwrap();
                let result=compile(text,&options);assert!(result.ok,"{:?}",result.diagnostics);
                check(result.code.as_ref().unwrap(),result.map.as_ref().unwrap());
            }
        }
    }
    #[test]
    fn streamed_linked_map_matches_the_released_format_with_multiple_sources() {
        let project:LuaProject=serde_json::from_value(serde_json::json!({"entry":"main","modules":{"main":"local f=require('helper') function onTick()output.setNumber(1,f(3))end","helper":"return function(v)return v*2 end"}})).unwrap();
        for minify in [false, true] {
            let options: ApiProjectCompileOptions =
                serde_json::from_value(serde_json::json!({"minify":minify,"sourceMap":true}))
                    .unwrap();
            let result = compile_project(&project, &options);
            assert!(result.ok, "{:?}", result.diagnostics);
            check(result.code.as_ref().unwrap(), result.map.as_ref().unwrap());
        }
    }
}
