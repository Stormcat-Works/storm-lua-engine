//! Borrowed serialization of the resolved compilation contract. This is the
//! same schema-1 metadata; caller-specific data is never globally cached.
use serde::{ser::SerializeMap, Serialize, Serializer};
use std::{collections::BTreeMap, sync::OnceLock};
use storm_lua_minify::config::{
    pass_enabled, resolve_folding_tolerance, resolve_literal_tolerance, resolve_pass_toggles,
    NumericTolerance, PropertyConfig, PropertyMode,
};
use storm_lua_minify::search::SearchStats;
use storm_lua_minify::{CompileMode, CompileOptions, NumericMode, SearchMode};

pub(crate) struct Settings<'a> {
    pub options: &'a CompileOptions,
    pub stats: Option<&'a SearchStats>,
}
struct Bits(f64);
impl Serialize for Bits {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        const HEX: &[u8; 16] = b"0123456789abcdef";
        let mut bytes = [b'0'; 16];
        let value = self.0.to_bits();
        for (i, b) in bytes.iter_mut().enumerate() {
            *b = HEX[((value >> ((15 - i) * 4)) & 15) as usize];
        }
        let text = std::str::from_utf8(&bytes).map_err(serde::ser::Error::custom)?;
        s.serialize_str(text)
    }
}
struct Tolerance(NumericTolerance);
impl Serialize for Tolerance {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut m = s.serialize_map(Some(2))?;
        m.serialize_entry("abs", &Bits(self.0.abs))?;
        m.serialize_entry("rel", &Bits(self.0.rel))?;
        m.end()
    }
}
struct Tolerances(Option<NumericTolerance>);
impl Serialize for Tolerances {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut m = s.serialize_map(Some(2))?;
        m.serialize_entry(
            "constantFolding",
            &Tolerance(resolve_folding_tolerance(self.0)),
        )?;
        m.serialize_entry(
            "literalApproximationBeforeCaps",
            &Tolerance(resolve_literal_tolerance(self.0)),
        )?;
        m.end()
    }
}
struct EffectivePasses<'a>(&'a CompileOptions);
impl Serialize for EffectivePasses<'_> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        static IDS: OnceLock<Vec<&'static str>> = OnceLock::new();
        let sorted = IDS.get_or_init(|| {
            let mut ids = storm_lua_minify::pass_ids::OPTIMIZATION_PASS_IDS.to_vec();
            ids.sort_unstable();
            ids
        });
        let resolved = resolve_pass_toggles(&self.0.pass_toggles, self.0.numeric_mode);
        let mut m = s.serialize_map(Some(sorted.len()))?;
        for &id in sorted {
            m.serialize_entry(id, &pass_enabled(&resolved, id))?;
        }
        m.end()
    }
}
struct Numbers<'a>(&'a BTreeMap<String, f64>);
impl Serialize for Numbers<'_> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut m = s.serialize_map(Some(self.0.len()))?;
        for (name, value) in self.0 {
            m.serialize_entry(name, &Bits(*value))?;
        }
        m.end()
    }
}
struct Property<'a>(&'a PropertyConfig);
impl Serialize for Property<'_> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let p = self.0;
        let mut m = s.serialize_map(Some(4))?;
        m.serialize_entry("bools", &p.bools)?;
        m.serialize_entry(
            "mode",
            match p.mode {
                PropertyMode::Runtime => "runtime",
                PropertyMode::Hardcode => "hardcode",
            },
        )?;
        m.serialize_entry("numbers", &p.numbers.as_ref().map(Numbers))?;
        m.serialize_entry("texts", &p.texts)?;
        m.end()
    }
}
struct Selection<'a>(&'a SearchStats);
impl Serialize for Selection<'_> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let v = self.0;
        let mut m = s.serialize_map(Some(5))?;
        m.serialize_entry(
            "criterion",
            if v.winner_structural == "lexical" {
                "conservative-token-only"
            } else if v.stopped_early {
                "valid-candidate-meeting-target"
            } else {
                "smallest-valid-evaluated-candidate"
            },
        )?;
        m.serialize_entry("layout", &v.winner_layout)?;
        m.serialize_entry("stoppedEarly", &v.stopped_early)?;
        m.serialize_entry("structural", &v.winner_structural)?;
        m.serialize_entry("targetMet", &v.target_met)?;
        m.end()
    }
}
impl Serialize for Settings<'_> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let o = self.options;
        let mut m = s.serialize_map(Some(15))?;
        m.serialize_entry(
            "effectiveNumericTolerances",
            &Tolerances(o.numeric_tolerance),
        )?;
        m.serialize_entry("effectivePassToggles", &EffectivePasses(o))?;
        m.serialize_entry("environment", &o.environment)?;
        m.serialize_entry("hostBindings", &o.host_bindings)?;
        m.serialize_entry(
            "mode",
            if o.mode == CompileMode::Safe {
                "safe"
            } else {
                "smallest"
            },
        )?;
        m.serialize_entry("numericEncoding", "ieee754-binary64-hex")?;
        m.serialize_entry(
            "numericMode",
            if o.numeric_mode == NumericMode::Exact {
                "exact"
            } else {
                "tolerant"
            },
        )?;
        m.serialize_entry("numericTolerance", &o.numeric_tolerance.map(Tolerance))?;
        m.serialize_entry("passToggles", &o.pass_toggles)?;
        m.serialize_entry("property", &o.property.as_ref().map(Property))?;
        m.serialize_entry("searchBeamWidth", &o.search_beam_width)?;
        m.serialize_entry(
            "searchMode",
            if o.search_mode == SearchMode::Fast {
                "fast"
            } else {
                "exhaustive"
            },
        )?;
        m.serialize_entry("selection", &self.stats.map(Selection))?;
        m.serialize_entry("targetSize", &o.target_size)?;
        m.serialize_entry("zeroCostNewlines", &o.zero_cost_newlines)?;
        m.end()
    }
}
