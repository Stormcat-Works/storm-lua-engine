//! Write the schema-1 canonical map without allocating a JSON object per record.
//! Trusted key/delimiter bytes are emitted in canonical order. All caller text,
//! numbers and dynamic values still pass through serde_json's normal encoder.
use crate::optimized_source_map::{MapDisposition, MapExtension, MapOrigin};
use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use storm_lua_syntax::explanation::{InlineContext, OptimizationReason, SourceRelation};
use storm_lua_syntax::provenance::{GeneratedConstruct, OriginMapping, SourceSpan};

type JsonResult = Result<(), serde_json::Error>;
#[derive(Default)]
struct Writer {
    bytes: Vec<u8>,
}
impl Writer {
    fn literal(&mut self, text: &'static [u8]) {
        self.bytes.extend_from_slice(text);
    }
    fn value<T: Serialize + ?Sized>(&mut self, value: &T) -> JsonResult {
        serde_json::to_writer(&mut self.bytes, value)
    }
    fn span(&mut self, value: &SourceSpan) -> JsonResult {
        self.literal(b"{\"end\":");
        self.value(&value.end)?;
        self.literal(b",\"source\":");
        self.value(&value.source)?;
        self.literal(b",\"start\":");
        self.value(&value.start)?;
        self.literal(b"}");
        Ok(())
    }
    fn optional_span(&mut self, value: Option<&SourceSpan>) -> JsonResult {
        match value {
            Some(value) => self.span(value),
            None => {
                self.literal(b"null");
                Ok(())
            }
        }
    }
    fn array<T>(&mut self, items: &[T], write: impl Fn(&mut Self, &T) -> JsonResult) -> JsonResult {
        self.literal(b"[");
        for (index, item) in items.iter().enumerate() {
            if index > 0 {
                self.literal(b",");
            }
            write(self, item)?;
        }
        self.literal(b"]");
        Ok(())
    }
    fn origin(&mut self, value: &MapOrigin) -> JsonResult {
        self.literal(b"{\"kind\":");
        self.value(&value.kind)?;
        self.literal(b",\"name\":");
        self.value(&value.name)?;
        self.literal(b",\"precision\":");
        self.value(&value.precision)?;
        self.literal(b",\"primary\":");
        self.optional_span(value.primary.as_ref())?;
        self.literal(b",\"reasons\":");
        self.value(&value.reasons)?;
        self.literal(b",\"related\":");
        self.value(&value.related)?;
        self.literal(b",\"transformation\":");
        self.value(&value.transformation)?;
        self.literal(b"}");
        Ok(())
    }
    fn mapping(&mut self, value: &OriginMapping) -> JsonResult {
        self.literal(b"{\"copied\":");
        self.optional_span(value.copied.as_ref())?;
        self.literal(b",\"end\":");
        self.value(&value.end)?;
        self.literal(b",\"inlineContexts\":");
        self.value(&value.inline_contexts)?;
        self.literal(b",\"origin\":");
        self.value(&value.origin)?;
        self.literal(b",\"start\":");
        self.value(&value.start)?;
        self.literal(b"}");
        Ok(())
    }
    fn construct(&mut self, value: &GeneratedConstruct) -> JsonResult {
        self.literal(b"{\"end\":");
        self.value(&value.end)?;
        self.literal(b",\"origin\":");
        self.value(&value.origin)?;
        self.literal(b",\"start\":");
        self.value(&value.start)?;
        self.literal(b"}");
        Ok(())
    }
    fn relation(&mut self, value: &SourceRelation) -> JsonResult {
        self.literal(b"{\"role\":");
        self.value(&value.role)?;
        self.literal(b",\"span\":");
        self.span(&value.span)?;
        self.literal(b"}");
        Ok(())
    }
    fn context(&mut self, value: &InlineContext) -> JsonResult {
        self.literal(b"{\"callSite\":");
        self.span(&value.call_site)?;
        self.literal(b",\"definition\":");
        self.span(&value.definition)?;
        self.literal(b"}");
        Ok(())
    }
    fn reason(&mut self, value: &OptimizationReason) -> JsonResult {
        self.literal(b"{\"after\":");
        self.value(&value.after)?;
        self.literal(b",\"basis\":");
        self.value(&value.basis)?;
        self.literal(b",\"before\":");
        self.value(&value.before)?;
        self.literal(b",\"code\":");
        self.value(&value.code)?;
        self.literal(b",\"facts\":");
        self.value(&value.facts)?;
        self.literal(b",\"operation\":");
        self.value(&value.operation)?;
        self.literal(b"}");
        Ok(())
    }
    fn disposition(&mut self, value: &MapDisposition) -> JsonResult {
        self.literal(b"{\"original\":");
        self.span(&value.original)?;
        self.literal(b",\"reason\":");
        self.value(&value.reason)?;
        self.literal(b",\"replacementSources\":");
        self.array(&value.replacement_sources, Self::span)?;
        self.literal(b"}");
        Ok(())
    }
    fn extension(&mut self, value: &MapExtension) -> Result<usize, serde_json::Error> {
        self.literal(b"{\"compilation\":");
        self.value(&value.compilation)?;
        self.literal(b",\"constructs\":");
        self.array(&value.constructs, Self::construct)?;
        self.literal(b",\"contexts\":");
        self.array(&value.contexts, Self::context)?;
        self.literal(b",\"coordinateUnit\":");
        self.value(&value.coordinate_unit)?;
        self.literal(b",\"dispositions\":");
        self.array(&value.dispositions, Self::disposition)?;
        self.literal(b",\"generated\":");
        self.value(&value.generated)?;
        self.literal(b",\"integrity\":\"");
        // Record the structural string position, never search source text for a
        // key. Reserve digest bytes now to avoid shifting the large suffix later.
        let digest_at = self.bytes.len();
        self.bytes.extend_from_slice(&[b'0'; 64]);
        self.literal(b"\",\"mappings\":");
        self.array(&value.mappings, Self::mapping)?;
        self.literal(b",\"origins\":");
        self.array(&value.origins, Self::origin)?;
        self.literal(b",\"producer\":{\"dirty\":");
        self.value(&value.producer.dirty)?;
        self.literal(b",\"name\":");
        self.value(&value.producer.name)?;
        self.literal(b",\"revision\":");
        self.value(&value.producer.revision)?;
        self.literal(b",\"version\":");
        self.value(&value.producer.version)?;
        self.literal(b"}");
        self.literal(b",\"reasons\":");
        self.array(&value.reasons, Self::reason)?;
        self.literal(b",\"relations\":");
        self.array(&value.relations, Self::relation)?;
        self.literal(b",\"schemaVersion\":");
        self.value(&value.schema_version)?;
        self.literal(b",\"sources\":");
        self.value(&value.sources)?;
        self.literal(b"}");
        Ok(digest_at)
    }
}

pub(crate) fn encode(standard: &Value, extension: MapExtension) -> Result<String, String> {
    let standard = standard
        .as_object()
        .ok_or("standard source map is not an object")?;
    // Reserve a conservative size based only on records already in memory.
    // Actual dynamic strings can still grow the buffer normally; this is not
    // an output limit and does not allocate or retain a second JSON copy.
    let capacity = 1024usize
        .saturating_add(extension.origins.len().saturating_mul(120))
        .saturating_add(extension.mappings.len().saturating_mul(64))
        .saturating_add(extension.constructs.len().saturating_mul(32));
    let mut writer = Writer {
        bytes: Vec::with_capacity(capacity),
    };
    writer.literal(b"{");
    for (name, value) in standard {
        writer.value(name).map_err(|e| e.to_string())?;
        writer.literal(b":");
        writer.value(value).map_err(|e| e.to_string())?;
        writer.literal(b",");
    }
    writer.literal(b"\"x_storm\":");
    let at = writer.extension(&extension).map_err(|e| e.to_string())?;
    writer.literal(b"}");
    // The canonical fingerprint is still computed with an EMPTY integrity
    // string. The reserved 64 characters are excluded from the hash input.
    let mut digest = Sha256::new();
    digest.update(&writer.bytes[..at]);
    digest.update(&writer.bytes[at + 64..]);
    let digest = digest.finalize();
    const HEX: &[u8; 16] = b"0123456789abcdef";
    for (i, byte) in digest.iter().copied().enumerate() {
        writer.bytes[at + 2 * i] = HEX[(byte >> 4) as usize];
        writer.bytes[at + 2 * i + 1] = HEX[(byte & 15) as usize];
    }
    String::from_utf8(writer.bytes).map_err(|e| e.to_string())
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
        // The independent Value serializer below includes every public struct
        // field, so omissions in the direct writer fail exact-byte equality.
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

    #[test]
    fn structural_integrity_hole_is_not_confused_by_lookalike_keys_or_source_text() {
        let source = "local text='integrity' function onTick()output.setNumber(1,2)end";
        let result = compile(
            source,
            &serde_json::from_value(serde_json::json!({"sourceMap":true,"targetSize":8192}))
                .unwrap(),
        );
        assert!(result.ok);
        let code = result.code.unwrap();
        let mut standard: Value = serde_json::from_str(&result.map.unwrap()).unwrap();
        let mut extension: MapExtension =
            serde_json::from_value(standard.as_object_mut().unwrap().remove("x_storm").unwrap())
                .unwrap();
        extension.compilation = serde_json::json!({"integrity":"", "inner":{"integrity":""}, "sourceText":"\"integrity\":\"\"雪😀"});
        extension.integrity.clear();
        let expected = old_encode(&standard, extension.clone());
        let actual = encode(&standard, extension).unwrap();
        assert_eq!(actual, expected);
        validate(&code, &actual).unwrap();
    }
}
