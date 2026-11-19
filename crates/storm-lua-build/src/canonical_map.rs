//! Write the schema-2 canonical map without allocating a JSON object per record.
//! Trusted key/delimiter bytes are emitted in canonical order. All caller text,
//! numbers and dynamic values still pass through serde_json's normal encoder.
use crate::map_settings::CompilationJson;
use crate::optimized_source_map::{
    BufferIdentity, FastMap, FastSet, MapDisposition, MapExtension, MapOrigin, MapProducer,
    SCHEMA_VERSION,
};
use serde::Serialize;
#[cfg(test)]
use serde_json::Value;
use sha2::{Digest, Sha256};
use storm_lua_syntax::explanation::{InlineContext, OptimizationReason, SourceRelation};
use storm_lua_syntax::provenance::{
    CopiedToken, GeneratedConstruct, LexicalCopiedOrigins, Origin, OriginMapping, SourceSpan,
};

type JsonResult = Result<(), serde_json::Error>;

#[derive(Clone, Copy)]
pub(crate) struct CompactTokenRecord {
    pub(crate) len: u8,
    pub(crate) values: [i64; 6],
}

type SimpleCopyTokenTable = (Vec<std::sync::Arc<str>>, Vec<CompactTokenRecord>);

#[derive(Default)]
pub(crate) struct Writer {
    pub(crate) bytes: Vec<u8>,
}
impl Writer {
    pub(crate) fn with_capacity(capacity: usize) -> Self {
        Self {
            bytes: Vec::with_capacity(capacity),
        }
    }
    pub(crate) fn literal(&mut self, text: &'static [u8]) {
        self.bytes.extend_from_slice(text);
    }
    pub(crate) fn value<T: Serialize + ?Sized>(&mut self, value: &T) -> JsonResult {
        serde_json::to_writer(&mut self.bytes, value)
    }
    fn compilation<C: CompilationJson + ?Sized>(&mut self, value: &C) -> JsonResult {
        value.write_compilation_json(&mut self.bytes)
    }
    fn usize_value(&mut self, mut value: usize) {
        let mut digits = [0u8; 20];
        let mut cursor = digits.len();
        loop {
            cursor -= 1;
            digits[cursor] = b'0' + (value % 10) as u8;
            value /= 10;
            if value == 0 {
                break;
            }
        }
        self.bytes.extend_from_slice(&digits[cursor..]);
    }
    fn u32_value(&mut self, value: u32) {
        self.usize_value(value as usize);
    }
    fn i64_value(&mut self, value: i64) {
        if value == -1 {
            self.literal(b"-1");
        } else if value < 0 {
            self.literal(b"-");
            self.usize_value(value.unsigned_abs() as usize);
        } else {
            self.usize_value(value as usize);
        }
    }
    fn u32_array(&mut self, items: &[u32]) {
        self.literal(b"[");
        for (index, &item) in items.iter().enumerate() {
            if index > 0 {
                self.literal(b",");
            }
            self.u32_value(item);
        }
        self.literal(b"]");
    }
    fn compact_tokens_array(&mut self, items: &[CompactTokenRecord]) {
        self.literal(b"[");
        for (index, record) in items.iter().enumerate() {
            if index > 0 {
                self.literal(b",");
            }
            self.literal(b"[");
            for (pos, &val) in record.values[..record.len as usize].iter().enumerate() {
                if pos > 0 {
                    self.literal(b",");
                }
                self.i64_value(val);
            }
            self.literal(b"]");
        }
        self.literal(b"]");
    }
    fn vec_tokens_array(&mut self, items: &[Vec<i64>]) {
        self.literal(b"[");
        for (index, record) in items.iter().enumerate() {
            if index > 0 {
                self.literal(b",");
            }
            self.literal(b"[");
            for (pos, &val) in record.iter().enumerate() {
                if pos > 0 {
                    self.literal(b",");
                }
                self.i64_value(val);
            }
            self.literal(b"]");
        }
        self.literal(b"]");
    }
    fn simple_span(&mut self, value: &SourceSpan) {
        self.literal(b"{\"end\":");
        self.usize_value(value.end);
        self.literal(b",\"source\":");
        self.u32_value(value.source);
        self.literal(b",\"start\":");
        self.usize_value(value.start);
        self.literal(b"}");
    }
    fn span(&mut self, value: &SourceSpan) -> JsonResult {
        self.simple_span(value);
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
    fn simple_copy_origin(&mut self, value: &Origin) -> JsonResult {
        debug_assert!(value.primary.is_some());
        debug_assert!(value.related.is_empty());
        debug_assert!(value.relations.is_empty());
        debug_assert!(value.reasons.is_empty());
        debug_assert!(value.transformation.is_none());
        self.literal(b"{\"kind\":\"source\",\"name\":");
        self.value(&value.name)?;
        self.literal(b",\"precision\":");
        match value.precision {
            storm_lua_syntax::provenance::OriginPrecision::Name => self.literal(b"\"name\""),
            storm_lua_syntax::provenance::OriginPrecision::Token => self.literal(b"\"token\""),
            _ => debug_assert!(false, "simple copy has non-lexical precision"),
        }
        self.literal(b",\"primary\":");
        if let Some(primary) = value.primary.as_ref() {
            self.simple_span(primary);
        } else {
            debug_assert!(false, "simple copy has no primary span");
            self.literal(b"null");
        }
        self.literal(b",\"reasons\":[],\"related\":[],\"transformation\":null}");
        Ok(())
    }
    fn simple_copy_mapping(&mut self, value: &OriginMapping) -> JsonResult {
        self.literal(b"{\"copied\":");
        if let Some(span) = value.copied.as_ref() {
            self.simple_span(span);
        } else {
            self.literal(b"null");
        }
        self.literal(b",\"end\":");
        self.usize_value(value.end);
        self.literal(b",\"inlineContexts\":[],\"origin\":");
        if let Some(origin) = value.origin {
            self.u32_value(origin);
        } else {
            self.literal(b"null");
        }
        self.literal(b",\"start\":");
        self.usize_value(value.start);
        self.literal(b"}");
        Ok(())
    }
    fn origin(&mut self, value: &MapOrigin) -> JsonResult {
        self.literal(b"{\"kind\":");
        // Unit enums serialize as fixed camelCase strings; writing the
        // literal avoids a serde call per origin without changing bytes.
        self.literal(match value.kind {
            storm_lua_syntax::provenance::OriginKind::Source => b"\"source\"",
            storm_lua_syntax::provenance::OriginKind::Derived => b"\"derived\"",
            storm_lua_syntax::provenance::OriginKind::Synthetic => b"\"synthetic\"",
        });
        self.literal(b",\"name\":");
        match value.name.as_ref() {
            None => self.literal(b"null"),
            Some(name) => self.value(name)?,
        }
        self.literal(b",\"precision\":");
        self.literal(match value.precision {
            storm_lua_syntax::provenance::OriginPrecision::Name => b"\"name\"",
            storm_lua_syntax::provenance::OriginPrecision::Token => b"\"token\"",
            storm_lua_syntax::provenance::OriginPrecision::Expression => b"\"expression\"",
            storm_lua_syntax::provenance::OriginPrecision::Statement => b"\"statement\"",
            storm_lua_syntax::provenance::OriginPrecision::Group => b"\"group\"",
        });
        self.literal(b",\"primary\":");
        self.optional_span(value.primary.as_ref())?;
        self.literal(b",\"reasons\":");
        self.u32_array(&value.reasons);
        self.literal(b",\"related\":");
        self.u32_array(&value.related);
        self.literal(b",\"transformation\":");
        match value.transformation.as_ref() {
            None => self.literal(b"null"),
            Some(transformation) => self.value(transformation)?,
        }
        self.literal(b"}");
        Ok(())
    }
    fn mapping(&mut self, value: &OriginMapping) -> JsonResult {
        self.literal(b"{\"copied\":");
        self.optional_span(value.copied.as_ref())?;
        self.literal(b",\"end\":");
        self.usize_value(value.end);
        self.literal(b",\"inlineContexts\":");
        self.u32_array(&value.inline_contexts);
        self.literal(b",\"origin\":");
        match value.origin {
            None => self.literal(b"null"),
            Some(id) => self.u32_value(id),
        }
        self.literal(b",\"start\":");
        self.usize_value(value.start);
        self.literal(b"}");
        Ok(())
    }
    fn construct(&mut self, value: &GeneratedConstruct) -> JsonResult {
        self.literal(b"{\"end\":");
        self.usize_value(value.end);
        self.literal(b",\"origin\":");
        match value.origin {
            None => self.literal(b"null"),
            Some(id) => self.u32_value(id),
        }
        self.literal(b",\"start\":");
        self.usize_value(value.start);
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
        match value.after.as_ref() {
            None => self.literal(b"null"),
            Some(after) => self.value(after)?,
        }
        self.literal(b",\"basis\":");
        match value.basis.as_ref() {
            None => self.literal(b"null"),
            Some(basis) => self.value(basis)?,
        }
        self.literal(b",\"before\":");
        match value.before.as_ref() {
            None => self.literal(b"null"),
            Some(before) => self.value(before)?,
        }
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
        self.simple_span(&value.original);
        self.literal(b",\"reason\":");
        self.u32_value(value.reason);
        self.literal(b",\"replacementSources\":");
        self.array(&value.replacement_sources, Self::span)?;
        self.literal(b"}");
        Ok(())
    }
    fn extension(
        &mut self,
        value: &MapExtension,
        compact_tokens: Option<&[CompactTokenRecord]>,
        compilation: &(impl CompilationJson + ?Sized),
    ) -> Result<usize, serde_json::Error> {
        self.literal(b"{\"compilation\":");
        self.compilation(compilation)?;
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
        self.u32_value(value.schema_version);
        self.literal(b",\"sources\":");
        self.value(&value.sources)?;
        self.literal(b",\"tokenNames\":");
        self.value(&value.token_names)?;
        self.literal(b",\"tokens\":");
        match compact_tokens {
            Some(tokens) => self.compact_tokens_array(tokens),
            None => self.vec_tokens_array(&value.tokens),
        }
        self.literal(b"}");
        Ok(digest_at)
    }

    #[allow(clippy::too_many_arguments)]
    fn composed_lexical_extension(
        &mut self,
        producer: &MapProducer,
        generated: &BufferIdentity,
        sources: &[BufferIdentity],
        token_names: &[&str],
        compact_tokens: &[CompactTokenRecord],
        residual_mappings: &[OriginMapping],
        residual_origin_count: usize,
        compilation: &(impl CompilationJson + ?Sized),
    ) -> Result<usize, serde_json::Error> {
        self.literal(b"{\"compilation\":");
        self.compilation(compilation)?;
        self.literal(b",\"constructs\":[],\"contexts\":[],\"coordinateUnit\":\"utf8-bytes\",\"dispositions\":[],\"generated\":");
        self.value(generated)?;
        self.literal(b",\"integrity\":\"");
        let digest_at = self.bytes.len();
        self.bytes.extend_from_slice(&[b'0'; 64]);
        if residual_origin_count == 0 {
            self.literal(b"\",\"mappings\":[],\"origins\":[]");
        } else {
            self.literal(b"\",\"mappings\":");
            self.array(residual_mappings, Self::mapping)?;
            self.literal(b",\"origins\":[");
            for idx in 0..residual_origin_count {
                if idx > 0 {
                    self.literal(b",");
                }
                self.literal(b"{\"kind\":\"synthetic\",\"name\":null,\"precision\":\"group\",\"primary\":null,\"reasons\":[0],\"related\":[],\"transformation\":\"linker-generated\"}");
            }
            self.literal(b"]");
        }
        self.literal(b",\"producer\":{\"dirty\":");
        self.value(&producer.dirty)?;
        self.literal(b",\"name\":");
        self.value(&producer.name)?;
        self.literal(b",\"revision\":");
        self.value(&producer.revision)?;
        self.literal(b",\"version\":");
        self.value(&producer.version)?;
        if residual_origin_count == 0 {
            self.literal(b"},\"reasons\":[],\"relations\":[],\"schemaVersion\":");
        } else {
            self.literal(b"},\"reasons\":[{\"after\":null,\"basis\":null,\"before\":null,\"code\":\"linker-generated\",\"facts\":[],\"operation\":\"synthesize\"}],\"relations\":[],\"schemaVersion\":");
        }
        self.u32_value(SCHEMA_VERSION);
        self.literal(b",\"sources\":");
        self.value(sources)?;
        self.literal(b",\"tokenNames\":[");
        for (index, name) in token_names.iter().enumerate() {
            if index > 0 {
                self.literal(b",");
            }
            self.value(name)?;
        }
        self.literal(b"],\"tokens\":");
        self.compact_tokens_array(compact_tokens);
        self.literal(b"}");
        Ok(digest_at)
    }

    fn lexical_copy_mapping_known(&mut self, token: &CopiedToken, origin: u32) {
        if token.renamed {
            // A renamed token is not an exact copy of its source span, so
            // the materialized form must not claim one (mirrors
            // `GeneratedOrigins::copied_tokens`).
            self.literal(b"{\"copied\":null,\"end\":");
        } else {
            self.literal(b"{\"copied\":{\"end\":");
            self.usize_value(token.source_end);
            self.literal(b",\"source\":0,\"start\":");
            self.usize_value(token.source_start);
            self.literal(b"},\"end\":");
        }
        self.usize_value(token.end());
        self.literal(b",\"inlineContexts\":[],\"origin\":");
        self.u32_value(origin);
        self.literal(b",\"start\":");
        self.usize_value(token.start);
        self.literal(b"}");
    }

    fn lexical_copy_mapping_unknown(&mut self, start: usize, end: usize) {
        self.literal(b"{\"copied\":null,\"end\":");
        self.usize_value(end);
        self.literal(b",\"inlineContexts\":[],\"origin\":null,\"start\":");
        self.usize_value(start);
        self.literal(b"}");
    }

    fn lexical_copy_extension_pooled(
        &mut self,
        producer: &MapProducer,
        generated: &BufferIdentity,
        sources: &[BufferIdentity],
        lexical: &LexicalCopiedOrigins,
        pool: LexicalNamePool<'_>,
        compilation: &(impl CompilationJson + ?Sized),
    ) -> Result<usize, serde_json::Error> {
        let LexicalNamePool {
            token_names,
            per_token_name_id,
        } = pool;
        // An empty token list cannot carry the unknown gap that spans the
        // whole generated code, so it keeps the materialized layout (with
        // empty token arrays) instead of an empty token table.
        if lexical.tokens.is_empty() {
            self.literal(b"{\"compilation\":");
            self.compilation(compilation)?;
            self.literal(b",\"constructs\":[],\"contexts\":[],\"coordinateUnit\":\"utf8-bytes\",\"dispositions\":[],\"generated\":");
            self.value(generated)?;
            self.literal(b",\"integrity\":\"");
            let digest_at = self.bytes.len();
            self.bytes.extend_from_slice(&[b'0'; 64]);
            self.literal(b"\",\"mappings\":[");
            let mut cursor = 0;
            let mut first = true;
            for (id, token) in lexical.tokens.iter().enumerate() {
                if cursor < token.start {
                    if !first {
                        self.literal(b",");
                    }
                    self.lexical_copy_mapping_unknown(cursor, token.start);
                    first = false;
                }
                if !first {
                    self.literal(b",");
                }
                self.lexical_copy_mapping_known(token, id as u32);
                first = false;
                cursor = token.end();
            }
            if cursor < lexical.generated_len {
                if !first {
                    self.literal(b",");
                }
                self.lexical_copy_mapping_unknown(cursor, lexical.generated_len);
            }
            self.literal(b"],\"origins\":[],\"producer\":{\"dirty\":");
            self.value(&producer.dirty)?;
            self.literal(b",\"name\":");
            self.value(&producer.name)?;
            self.literal(b",\"revision\":");
            self.value(&producer.revision)?;
            self.literal(b",\"version\":");
            self.value(&producer.version)?;
            self.literal(b"},\"reasons\":[],\"relations\":[],\"schemaVersion\":");
            self.u32_value(SCHEMA_VERSION);
            self.literal(b",\"sources\":");
            self.value(sources)?;
            self.literal(b",\"tokenNames\":[],\"tokens\":[]}");
            return Ok(digest_at);
        }
        // Schema-2 token table: reuse the first-appearance name pool and
        // per-token name IDs already computed for the standard map fields.
        self.literal(b"{\"compilation\":");
        self.compilation(compilation)?;
        self.literal(b",\"constructs\":[],\"contexts\":[],\"coordinateUnit\":\"utf8-bytes\",\"dispositions\":[],\"generated\":");
        self.value(generated)?;
        self.literal(b",\"integrity\":\"");
        let digest_at = self.bytes.len();
        self.bytes.extend_from_slice(&[b'0'; 64]);
        self.literal(b"\",\"mappings\":[],\"origins\":[],\"producer\":{\"dirty\":");
        self.value(&producer.dirty)?;
        self.literal(b",\"name\":");
        self.value(&producer.name)?;
        self.literal(b",\"revision\":");
        self.value(&producer.revision)?;
        self.literal(b",\"version\":");
        self.value(&producer.version)?;
        self.literal(b"},\"reasons\":[],\"relations\":[],\"schemaVersion\":");
        self.u32_value(SCHEMA_VERSION);
        self.literal(b",\"sources\":");
        self.value(sources)?;
        self.literal(b",\"tokenNames\":[");
        for (index, name) in token_names.iter().enumerate() {
            if index > 0 {
                self.literal(b",");
            }
            self.value(name)?;
        }
        self.literal(b"],\"tokens\":[");
        for (index, token) in lexical.tokens.iter().enumerate() {
            if index > 0 {
                self.literal(b",");
            }
            self.literal(b"[");
            self.usize_value(token.start);
            self.literal(b",");
            self.usize_value(token.source_start);
            self.literal(b",");
            self.usize_value(token.source_end);
            self.literal(b",");
            let nid = per_token_name_id[index];
            if nid != u32::MAX {
                self.u32_value(nid);
            } else {
                self.literal(b"-1");
            }
            if token.renamed {
                self.literal(b",");
                self.usize_value(token.end());
            }
            self.literal(b"]");
        }
        self.literal(b"]}");
        Ok(digest_at)
    }

    /// Schema-2 token-table eligibility for a lexical-shape (simple-copy)
    /// map. Returns the name pool and token records when the table expands
    /// back to exactly these origins and mappings, and `None` otherwise
    /// (the caller then emits the traditional layout under schema 2).
    /// The checks are deliberately strict: every origin
    /// referenced at least once, first-appearance order equal to origin
    /// order, unique (source, primary, name, precision) tuples, name present
    /// exactly for name-precision origins, and mappings that contiguously
    /// cover the generated code so unknown gaps are exactly the gaps
    /// between known tokens. Multiple sources are supported: the record
    /// layout then carries an explicit source index (see the token-table
    /// documentation on `MapExtension::tokens`); single-source maps keep
    /// the original record layout byte-for-byte.
    fn simple_copy_token_table(
        sources_len: usize,
        origins: &[Origin],
        mappings: &[OriginMapping],
        generated_len: usize,
    ) -> Option<SimpleCopyTokenTable> {
        if sources_len == 0 || origins.is_empty() || mappings.is_empty() {
            return None;
        }
        let mut seen_tuples: FastSet<(u32, usize, usize, Option<&str>, bool)> =
            FastSet::with_capacity_and_hasher(origins.len(), Default::default());
        for origin in origins {
            if origin.kind != storm_lua_syntax::provenance::OriginKind::Source
                || origin.transformation.is_some()
                || !origin.related.is_empty()
                || !origin.relations.is_empty()
                || !origin.reasons.is_empty()
            {
                return None;
            }
            let primary = origin.primary?;
            if primary.source as usize >= sources_len {
                return None;
            }
            let is_name = matches!(
                origin.precision,
                storm_lua_syntax::provenance::OriginPrecision::Name
            );
            let is_token = matches!(
                origin.precision,
                storm_lua_syntax::provenance::OriginPrecision::Token
            );
            if !is_name && !is_token {
                return None;
            }
            if is_name != origin.name.is_some() {
                return None;
            }
            if !seen_tuples.insert((
                primary.source,
                primary.start,
                primary.end,
                origin.name.as_deref(),
                is_name,
            )) {
                return None;
            }
        }
        let mut next_expected_origin = 0u32;
        let mut cursor = 0usize;
        for mapping in mappings {
            if mapping.start != cursor || mapping.end < mapping.start {
                return None;
            }
            cursor = mapping.end;
            if !mapping.inline_contexts.is_empty() {
                return None;
            }
            if let Some(id) = mapping.origin {
                let origin = origins.get(id as usize)?;
                match mapping.copied {
                    Some(copied) => {
                        if Some(copied) != origin.primary {
                            return None;
                        }
                        if mapping.end - mapping.start != copied.end - copied.start {
                            return None;
                        }
                    }
                    None => {
                        if !matches!(
                            origin.precision,
                            storm_lua_syntax::provenance::OriginPrecision::Name
                        ) || origin.name.is_none()
                        {
                            return None;
                        }
                    }
                }
                if id == next_expected_origin {
                    next_expected_origin += 1;
                } else if id > next_expected_origin {
                    return None;
                }
            } else if mapping.copied.is_some() {
                return None;
            }
        }
        if cursor != generated_len || next_expected_origin as usize != origins.len() {
            return None;
        }
        let mut name_ids: FastMap<&str, usize> =
            FastMap::with_capacity_and_hasher(256, Default::default());
        let mut token_names: Vec<std::sync::Arc<str>> = Vec::new();
        for origin in origins {
            if let Some(name) = &origin.name {
                name_ids.entry(name.as_ref()).or_insert_with(|| {
                    let id = token_names.len();
                    token_names.push(std::sync::Arc::clone(name));
                    id
                });
            }
        }
        let mut tokens: Vec<CompactTokenRecord> = Vec::with_capacity(mappings.len());
        for mapping in mappings {
            if let Some(id) = mapping.origin {
                let origin = &origins[id as usize];
                let name_id = match &origin.name {
                    Some(name) => name_ids[name.as_ref()] as i64,
                    None => -1,
                };
                match mapping.copied {
                    Some(copied) => {
                        if sources_len == 1 {
                            tokens.push(CompactTokenRecord {
                                len: 4,
                                values: [
                                    mapping.start as i64,
                                    copied.start as i64,
                                    copied.end as i64,
                                    name_id,
                                    0,
                                    0,
                                ],
                            });
                        } else {
                            tokens.push(CompactTokenRecord {
                                len: 5,
                                values: [
                                    mapping.start as i64,
                                    copied.source as i64,
                                    copied.start as i64,
                                    copied.end as i64,
                                    name_id,
                                    0,
                                ],
                            });
                        }
                    }
                    None => {
                        let primary = origin.primary?;
                        if sources_len == 1 {
                            tokens.push(CompactTokenRecord {
                                len: 5,
                                values: [
                                    mapping.start as i64,
                                    primary.start as i64,
                                    primary.end as i64,
                                    name_id,
                                    mapping.end as i64,
                                    0,
                                ],
                            });
                        } else {
                            tokens.push(CompactTokenRecord {
                                len: 6,
                                values: [
                                    mapping.start as i64,
                                    primary.source as i64,
                                    primary.start as i64,
                                    primary.end as i64,
                                    name_id,
                                    mapping.end as i64,
                                ],
                            });
                        }
                    }
                }
            }
        }
        Some((token_names, tokens))
    }

    fn simple_copy_extension(
        &mut self,
        producer: &MapProducer,
        generated: &BufferIdentity,
        sources: &[BufferIdentity],
        origins: &[Origin],
        mappings: &[OriginMapping],
        compilation: &(impl CompilationJson + ?Sized),
    ) -> Result<usize, serde_json::Error> {
        let token_table =
            Self::simple_copy_token_table(sources.len(), origins, mappings, generated.bytes);
        self.literal(b"{\"compilation\":");
        self.compilation(compilation)?;
        self.literal(b",\"constructs\":[],\"contexts\":[],\"coordinateUnit\":\"utf8-bytes\",\"dispositions\":[],\"generated\":");
        self.value(generated)?;
        self.literal(b",\"integrity\":\"");
        let digest_at = self.bytes.len();
        self.bytes.extend_from_slice(&[b'0'; 64]);
        if token_table.is_some() {
            self.literal(b"\",\"mappings\":[],\"origins\":[]");
        } else {
            self.literal(b"\",\"mappings\":");
            self.array(mappings, Self::simple_copy_mapping)?;
            self.literal(b",\"origins\":");
            self.array(origins, Self::simple_copy_origin)?;
        }
        self.literal(b",\"producer\":{\"dirty\":");
        self.value(&producer.dirty)?;
        self.literal(b",\"name\":");
        self.value(&producer.name)?;
        self.literal(b",\"revision\":");
        self.value(&producer.revision)?;
        self.literal(b",\"version\":");
        self.value(&producer.version)?;
        self.literal(b"},\"reasons\":[],\"relations\":[],\"schemaVersion\":");
        self.u32_value(SCHEMA_VERSION);
        self.literal(b",\"sources\":");
        self.value(sources)?;
        match &token_table {
            Some((token_names, tokens)) => {
                self.literal(b",\"tokenNames\":");
                self.value(token_names)?;
                self.literal(b",\"tokens\":");
                self.compact_tokens_array(tokens);
            }
            None => {
                self.literal(b",\"tokenNames\":[],\"tokens\":[]");
            }
        }
        self.literal(b"}");
        Ok(digest_at)
    }
}

pub(crate) fn finish(mut writer: Writer, digest_at: usize) -> Result<String, String> {
    writer.literal(b"}");
    let mut digest = Sha256::new();
    digest.update(&writer.bytes[..digest_at]);
    digest.update(&writer.bytes[digest_at + 64..]);
    let digest = digest.finalize();
    const HEX: &[u8; 16] = b"0123456789abcdef";
    for (i, byte) in digest.iter().copied().enumerate() {
        writer.bytes[digest_at + 2 * i] = HEX[(byte >> 4) as usize];
        writer.bytes[digest_at + 2 * i + 1] = HEX[(byte & 15) as usize];
    }
    String::from_utf8(writer.bytes).map_err(|e| e.to_string())
}

#[cfg(test)]
fn begin_standard(standard: &(impl Serialize + ?Sized), capacity: usize) -> Result<Writer, String> {
    let mut writer = Writer {
        bytes: Vec::with_capacity(capacity),
    };
    writer.value(standard).map_err(|e| e.to_string())?;
    if writer.bytes.first() != Some(&b'{') || writer.bytes.last() != Some(&b'}') {
        return Err("standard source map is not an object".into());
    }
    writer.bytes.pop();
    if writer.bytes.len() > 1 {
        writer.literal(b",");
    }
    writer.literal(b"\"x_storm\":");
    Ok(writer)
}

#[derive(Clone, Copy)]
pub(crate) struct LexicalNamePool<'a> {
    pub token_names: &'a [&'a str],
    pub per_token_name_id: &'a [u32],
}

pub(crate) fn finish_lexical_copy_with_settings(
    mut writer: Writer,
    producer: &MapProducer,
    generated: &BufferIdentity,
    sources: &[BufferIdentity],
    lexical: &LexicalCopiedOrigins,
    pool: LexicalNamePool<'_>,
    compilation: &(impl CompilationJson + ?Sized),
) -> Result<String, String> {
    let at = writer
        .lexical_copy_extension_pooled(producer, generated, sources, lexical, pool, compilation)
        .map_err(|e| e.to_string())?;
    finish(writer, at)
}

pub(crate) fn finish_simple_copy_with_settings(
    mut writer: Writer,
    producer: &MapProducer,
    generated: &BufferIdentity,
    sources: &[BufferIdentity],
    origins: &[Origin],
    mappings: &[OriginMapping],
    compilation: &(impl CompilationJson + ?Sized),
) -> Result<String, String> {
    let at = writer
        .simple_copy_extension(producer, generated, sources, origins, mappings, compilation)
        .map_err(|e| e.to_string())?;
    finish(writer, at)
}

#[cfg(test)]
pub(crate) fn encode(standard: &Value, mut extension: MapExtension) -> Result<String, String> {
    let compilation = std::mem::take(&mut extension.compilation);
    let mut writer = begin_standard(standard, 4096)?;
    let at = writer
        .extension(&extension, None, &compilation)
        .map_err(|e| e.to_string())?;
    finish(writer, at)
}

pub(crate) fn finish_with_compact_tokens(
    mut writer: Writer,
    extension: MapExtension,
    compact_tokens: Option<&[CompactTokenRecord]>,
    compilation: &(impl CompilationJson + ?Sized),
) -> Result<String, String> {
    let at = writer
        .extension(&extension, compact_tokens, compilation)
        .map_err(|e| e.to_string())?;
    finish(writer, at)
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn finish_composed_lexical_with_settings(
    mut writer: Writer,
    producer: &MapProducer,
    generated: &BufferIdentity,
    sources: &[BufferIdentity],
    token_names: &[&str],
    compact_tokens: &[CompactTokenRecord],
    residual_mappings: &[OriginMapping],
    residual_origin_count: usize,
    compilation: &(impl CompilationJson + ?Sized),
) -> Result<String, String> {
    let at = writer
        .composed_lexical_extension(
            producer,
            generated,
            sources,
            token_names,
            compact_tokens,
            residual_mappings,
            residual_origin_count,
            compilation,
        )
        .map_err(|e| e.to_string())?;
    finish(writer, at)
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

    // Independent reference of the released schema-2 materialized encoder. It
    // hashes sorted JSON with an empty integrity field, then writes it again.
    fn old_encode(standard: &Value, mut extension: MapExtension) -> String {
        extension.integrity.clear();
        let mut value = standard.clone();
        value["x_storm"] = serde_json::to_value(extension).unwrap();
        let hash = Sha256::digest(serde_json::to_vec(&value).unwrap())
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
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
        let expected_mappings = serde_json::to_value(&extension.mappings).unwrap();
        let expected_origins = serde_json::to_value(&extension.origins).unwrap();
        let expected = old_encode(&original, extension.clone());
        let actual = encode(&original, extension).unwrap();
        assert_eq!(
            actual, expected,
            "exact canonical bytes and fingerprint differ"
        );
        // A schema-2 token-table wire map re-encodes to the materialized
        // form: the bytes differ by construction, but validating the
        // re-encoded map must expand to exactly the same details.
        let wire_tokens = serde_json::from_str::<Value>(map).unwrap()["x_storm"]["tokens"]
            .as_array()
            .is_none_or(|tokens| tokens.is_empty());
        if wire_tokens {
            assert_eq!(actual, map, "regenerated map differs");
        } else {
            let revalidated = validate(code, &actual).unwrap();
            assert_eq!(
                serde_json::to_value(&revalidated.mappings).unwrap(),
                expected_mappings,
                "re-encoded token map expands to different mappings"
            );
            assert_eq!(
                serde_json::to_value(&revalidated.origins).unwrap(),
                expected_origins,
                "re-encoded token map expands to different origins"
            );
        }
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
