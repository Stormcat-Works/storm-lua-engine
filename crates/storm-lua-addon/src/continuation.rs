//! Idle Addon continuation. Source is retained for a later explicit reload, never executed here.
use super::*;
use storm_lua_spec::property::PropertyValue;

const MAGIC: &[u8] = b"SLADDON\0\x02\0\0\0";
const MAGIC_V1: &[u8] = b"SLADDON\0\x01\0\0\0";
const MAX_BYTES: usize = 256 * 1024 * 1024;

fn put(out: &mut Vec<u8>, value: &[u8]) -> Result<(), VmError> {
    let len = u32::try_from(value.len()).map_err(|_| invalid("Addon section too large"))?;
    if out.len().saturating_add(4).saturating_add(value.len()) > MAX_BYTES {
        return Err(invalid("Addon continuation too large"));
    }
    out.extend_from_slice(&len.to_le_bytes());
    out.extend_from_slice(value);
    Ok(())
}
struct Reader<'a>(&'a [u8]);
impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], VmError> {
        let value = self
            .0
            .get(..n)
            .ok_or_else(|| invalid("truncated Addon continuation"))?;
        self.0 = &self.0[n..];
        Ok(value)
    }
    fn byte(&mut self) -> Result<u8, VmError> {
        Ok(self.take(1)?[0])
    }
    fn boolean(&mut self) -> Result<bool, VmError> {
        match self.byte()? {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(invalid("invalid Boolean")),
        }
    }
    fn u32(&mut self) -> Result<u32, VmError> {
        Ok(u32::from_le_bytes(
            self.take(4)?
                .try_into()
                .map_err(|_| invalid("invalid u32"))?,
        ))
    }
    fn u64(&mut self) -> Result<u64, VmError> {
        let b = self.take(8)?;
        Ok(u64::from_le_bytes([
            b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7],
        ]))
    }
    fn number(&mut self) -> Result<f64, VmError> {
        Ok(f64::from_le_bytes(self.u64()?.to_le_bytes()))
    }
    fn bytes(&mut self) -> Result<&'a [u8], VmError> {
        let len = self.u32()? as usize;
        self.take(len)
    }
}

impl Addon {
    /// Capture the heap, lifecycle, properties and error state at an idle boundary.
    /// Outstanding built-in HTTP requests and development require caches are refused.
    /// Host services (including an installed CRT stream) remain the host's responsibility.
    pub fn snapshot(&self) -> Result<Vec<u8>, VmError> {
        let vm = &self.vm;
        vm.ensure_idle()?;
        if vm.log_reader().has_pending() {
            return Err(VmError::new(
                ErrorKind::Unsupported,
                "Addon continuation requires drained log records",
            ));
        }
        let stage = match stage_of(&self.stage) {
            Stage::Empty => 0,
            Stage::Loaded => 1,
            Stage::Ready => 2,
            Stage::Destroyed => 3,
            _ => return Err(invalid("Addon continuation requires an idle lifecycle")),
        };
        if self.config.require_loader.is_some() {
            return Err(VmError::new(
                ErrorKind::Unsupported,
                "Addon development require cache has no continuation",
            ));
        }
        let mut out = MAGIC.to_vec();
        {
            let state = lock_state(&self.state);
            let cursor = state.http.continuation_cursor().map_err(|_| {
                VmError::new(
                    ErrorKind::Unsupported,
                    "Addon continuation requires drained HTTP requests",
                )
            })?;
            out.extend_from_slice(&[
                stage,
                u8::try_from(self.error_state.status())
                    .map_err(|_| invalid("invalid Addon error status"))?,
            ]);
            let error_log = self.error_state.log();
            out.extend_from_slice(
                &u32::try_from(error_log.len())
                    .map_err(|_| invalid("too many Addon error records"))?
                    .to_le_bytes(),
            );
            for message in &error_log {
                put(&mut out, message.as_bytes())?;
            }
            out.push(u8::from(state.new_world));
            out.extend_from_slice(&cursor.to_le_bytes());
            put(&mut out, &self.source)?;
            put(&mut out, self.source_name.as_bytes())?;
            out.extend_from_slice(
                &u32::try_from(state.properties.len())
                    .map_err(|_| invalid("too many properties"))?
                    .to_le_bytes(),
            );
            for (name, value) in state.properties.iter() {
                put(&mut out, name)?;
                match value {
                    PropertyValue::Number(n) => {
                        out.push(0);
                        out.extend_from_slice(&n.to_le_bytes());
                    }
                    PropertyValue::Bool(b) => {
                        out.extend_from_slice(&[1, u8::from(*b)]);
                    }
                    PropertyValue::Text(s) => {
                        out.push(2);
                        put(&mut out, s)?;
                    }
                }
            }
            out.extend_from_slice(&(state.definitions.len() as u32).to_le_bytes());
            for (name, value) in &state.definitions {
                put(&mut out, name)?;
                match value {
                    MenuProperty::Checkbox(b) => out.extend_from_slice(&[0, u8::from(*b)]),
                    MenuProperty::Slider(ns) => {
                        out.push(1);
                        for n in ns {
                            out.extend_from_slice(&n.to_le_bytes());
                        }
                    }
                }
            }
        }
        let random_state = vm.random_stream_state()?;
        put(&mut out, &vm.snapshot()?)?;
        match random_state {
            storm_lua_vm::runner::RandomStreamState::HostOwned => out.push(0),
            storm_lua_vm::runner::RandomStreamState::Engine(seed) => {
                out.push(1);
                out.extend_from_slice(&seed.to_le_bytes());
            }
            storm_lua_vm::runner::RandomStreamState::BuiltinCrt(word) => {
                out.push(2);
                out.extend_from_slice(&word.to_le_bytes());
            }
        }
        Ok(out)
    }

    /// Construct fresh host bindings and restore without loading source or calling onCreate.
    /// Supply the same host API/profile/limits as capture. Old callback handles are not reused.
    /// A failed candidate is dropped; no partially restored Addon is returned.
    pub fn from_snapshot(mut config: AddonConfig, bytes: &[u8]) -> Result<Self, VmError> {
        if bytes.len() > MAX_BYTES {
            return Err(invalid("Addon continuation too large"));
        }
        if config.savedata.is_some() || config.require_loader.is_some() {
            return Err(invalid(
                "continuation cannot apply savedata or a development require cache",
            ));
        }
        let mut r = Reader(bytes);
        let header = r.take(MAGIC.len())?;
        if header == MAGIC_V1 {
            return Err(VmError::new(
                ErrorKind::Unsupported,
                "unsupported addon continuation version",
            ));
        }
        if header != MAGIC {
            return Err(invalid("unsupported Addon continuation"));
        }
        let stage = match r.byte()? {
            0 => Stage::Empty,
            1 => Stage::Loaded,
            2 => Stage::Ready,
            3 => Stage::Destroyed,
            _ => return Err(invalid("invalid Addon lifecycle")),
        };
        let error_status = r.byte()?;
        if !matches!(error_status, 0 | 2 | 3 | 5) {
            return Err(invalid("invalid Addon error status"));
        }
        let error_count = r.u32()?;
        if error_count > 11 {
            return Err(invalid("too many Addon error records"));
        }
        let mut error_log = Vec::new();
        for _ in 0..error_count {
            let message = r.bytes()?;
            error_log.push(
                std::str::from_utf8(message)
                    .map_err(|_| invalid("invalid Addon error record"))?
                    .to_owned(),
            );
        }
        let new_world = r.boolean()?;
        let http = HttpQueue::from_continuation_cursor(r.u32()?).map_err(http_error)?;
        let source_slice = r.bytes()?;
        let source_name_slice = r.bytes()?;
        if source_name_slice.len() > 1024 {
            return Err(invalid("invalid source size"));
        }
        if let Some(limit) = config.limits.max_source_bytes {
            if source_slice.len() > limit.get() {
                return Err(invalid("invalid source size"));
            }
        }
        let source_name = std::str::from_utf8(source_name_slice)
            .map_err(|_| invalid("invalid source name"))?
            .to_owned();
        if source_name.contains('\0') {
            return Err(invalid("invalid source name"));
        }
        match stage {
            Stage::Empty if !source_slice.is_empty() || !source_name.is_empty() => {
                return Err(invalid(
                    "empty Addon lifecycle cannot carry retained source",
                ));
            }
            Stage::Loaded | Stage::Ready | Stage::Destroyed if source_name.is_empty() => {
                return Err(invalid("loaded Addon lifecycle requires a source name"));
            }
            _ => {}
        }
        let source = source_slice.to_vec();
        let mut properties = PropertyBag::default();
        let count = r.u32()?;
        if count > 1_048_576 {
            return Err(invalid("too many properties"));
        }
        for _ in 0..count {
            let name = r.bytes()?.to_vec();
            let value = match r.byte()? {
                0 => PropertyValue::Number(r.number()?),
                1 => PropertyValue::Bool(r.boolean()?),
                2 => PropertyValue::Text(r.bytes()?.to_vec()),
                _ => return Err(invalid("invalid property")),
            };
            if properties.insert(name, value).is_some() {
                return Err(invalid("duplicate property"));
            }
        }
        let mut definitions = BTreeMap::new();
        let count = r.u32()?;
        if count > 4096 {
            return Err(invalid("too many menu definitions"));
        }
        for _ in 0..count {
            let name = r.bytes()?.to_vec();
            let value = match r.byte()? {
                0 => MenuProperty::Checkbox(r.boolean()?),
                // E12: declarations carry no range/step/finiteness contract in
                // the real game, so restored definitions are stored as-is.
                // The definition count cap above remains the size guard.
                1 => MenuProperty::Slider([r.number()?, r.number()?, r.number()?, r.number()?]),
                _ => return Err(invalid("invalid menu definition")),
            };
            if definitions.insert(name, value).is_some() {
                return Err(invalid("duplicate definition"));
            }
        }
        let heap = r.bytes()?;
        let random_state = match r.byte()? {
            0 => storm_lua_vm::runner::RandomStreamState::HostOwned,
            1 => storm_lua_vm::runner::RandomStreamState::Engine(r.u64()?),
            2 => storm_lua_vm::runner::RandomStreamState::BuiltinCrt(r.u32()?),
            _ => return Err(invalid("invalid random stream state")),
        };
        if !r.0.is_empty() {
            return Err(invalid("trailing Addon continuation"));
        }
        config.is_world_create = new_world;
        config.properties = properties.clone();
        let mut addon = Self::new(config)?;
        // 所有形態が復元先VMの系統と一致しなければ、黙って別系統の列へ差し替えない。
        // 系統の既定はプロファイルにも依存するため（Extended既定はEngine系統）、
        // 設定フラグではなく構築済みVMの実際の系統と比較する。
        let kind_matches = matches!(
            (addon.vm.random_stream_state()?, &random_state),
            (
                storm_lua_vm::runner::RandomStreamState::HostOwned,
                storm_lua_vm::runner::RandomStreamState::HostOwned
            ) | (
                storm_lua_vm::runner::RandomStreamState::Engine(_),
                storm_lua_vm::runner::RandomStreamState::Engine(_)
            ) | (
                storm_lua_vm::runner::RandomStreamState::BuiltinCrt(_),
                storm_lua_vm::runner::RandomStreamState::BuiltinCrt(_)
            )
        );
        if !kind_matches {
            return Err(invalid(
                "Addon continuation random stream ownership does not match AddonConfig",
            ));
        }
        addon.vm.restore(heap)?;
        match random_state {
            storm_lua_vm::runner::RandomStreamState::Engine(_)
            | storm_lua_vm::runner::RandomStreamState::BuiltinCrt(_) => {
                addon.vm.set_random_stream_state(random_state)?;
            }
            storm_lua_vm::runner::RandomStreamState::HostOwned => {}
        }
        *lock_state(&addon.state) = State {
            new_world,
            properties,
            definitions,
            http,
        };
        addon.source = source;
        addon.source_name = source_name;
        *storm_lua_vm::runner::lock(&addon.stage) = stage;
        addon
            .error_state
            .restore(u32::from(error_status), error_log);
        Ok(addon)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    #[test]
    fn ready_addon_continues_shared_upvalue_without_create_or_source() {
        let creates = Arc::new(AtomicUsize::new(0));
        let mut config = AddonConfig::default();
        let observed = Arc::clone(&creates);
        config.server.insert(
            "created".into(),
            Arc::new(move |_| {
                observed.fetch_add(1, Ordering::Relaxed);
                Ok(vec![])
            }),
        );
        let mut addon = Addon::new(config.clone()).unwrap();
        addon
            .load(
                br#"
            property.checkbox('switch', true)
            local n = 0
            function onCreate() server.created() end
            function onTick() n=n+1; g_savedata.counter=n end
            function onCustomCommand() n=n+10 end
        "#,
                "continuation",
            )
            .unwrap();
        addon.start().unwrap();
        addon.tick(1).unwrap();
        addon.tick(1).unwrap();
        let bytes = addon.snapshot().unwrap();
        let old = addon.synchronous_events();
        drop(addon);
        let mut addon = Addon::from_snapshot(config, &bytes).unwrap();
        assert_eq!(creates.load(Ordering::Relaxed), 1);
        assert!(!old.is_live());
        assert_eq!(addon.property_definitions().len(), 1);
        addon.dispatch("onCustomCommand", &[]).unwrap();
        addon.tick(1).unwrap();
        assert_eq!(
            addon.savedata().unwrap(),
            LuaValue::Table(vec![(LuaValue::text("counter"), LuaValue::Integer(13))])
        );
        assert_eq!(creates.load(Ordering::Relaxed), 1);
    }
    #[test]
    fn tick_error_latch_and_loaded_lifecycle_survive() {
        let mut addon = Addon::new(Default::default()).unwrap();
        addon.load(b"function onTick() error('stop') end; function onCustomCommand() g_savedata.ok=true end", "error").unwrap();
        let bytes = addon.snapshot().unwrap();
        drop(addon);
        let mut addon = Addon::from_snapshot(Default::default(), &bytes).unwrap();
        assert!(addon.tick(1).is_err());
        addon.start().unwrap();
        assert!(addon.tick(1).is_err());
        let bytes = addon.snapshot().unwrap();
        drop(addon);
        let mut addon = Addon::from_snapshot(Default::default(), &bytes).unwrap();
        assert_eq!(addon.tick(1).unwrap(), RunOutcome::Missing);
        addon.dispatch("onCustomCommand", &[]).unwrap();
        addon.destroy().unwrap();
        let bytes = addon.snapshot().unwrap();
        assert!(Addon::from_snapshot(Default::default(), &bytes)
            .unwrap()
            .tick(1)
            .is_err());
    }
    #[test]
    fn scoped_function_alias_rebinds_to_the_fresh_host_phase() {
        let config = AddonConfig {
            scoped_server: vec!["readNumber".into()],
            ..Default::default()
        };
        let mut addon = Addon::new(config.clone()).unwrap();
        addon
            .load(
                b"local read=server.readNumber; function onTick() g_savedata.value=read() end",
                "scoped",
            )
            .unwrap();
        addon.start().unwrap();
        let old = |_: &[LuaValue]| Ok(vec![LuaValue::Integer(17)]);
        let target = addon.callback_target();
        target
            .with_server_functions(&[("readNumber", &old)], || addon.tick(1))
            .unwrap();
        let bytes = addon.snapshot().unwrap();
        drop(addon);
        let mut restored = Addon::from_snapshot(config, &bytes).unwrap();
        let new = |_: &[LuaValue]| Ok(vec![LuaValue::Integer(29)]);
        restored
            .callback_target()
            .with_server_functions(&[("readNumber", &new)], || restored.tick(1))
            .unwrap();
        assert_eq!(
            restored.savedata().unwrap(),
            LuaValue::record([("value", LuaValue::Integer(29))])
        );
    }
    #[test]
    fn pending_logs_must_be_delivered_before_continuation() {
        let mut addon = Addon::new(Default::default()).unwrap();
        addon
            .load(b"debug.log('keep this record')", "logs")
            .unwrap();
        assert_eq!(addon.snapshot().unwrap_err().kind, ErrorKind::Unsupported);
        assert_eq!(addon.drain_log_records().len(), 1);
        addon.snapshot().unwrap();
    }
    #[test]
    fn outstanding_http_and_corrupt_images_are_refused() {
        let mut addon = Addon::new(Default::default()).unwrap();
        let bytes = addon.snapshot().unwrap();
        for n in 0..bytes.len() {
            assert!(Addon::from_snapshot(Default::default(), &bytes[..n]).is_err());
        }
        addon
            .load(b"server.httpGet(8080, '/pending')", "http")
            .unwrap();
        assert_eq!(addon.snapshot().unwrap_err().kind, ErrorKind::Unsupported);
    }

    #[test]
    fn bug_t2_02_addon_host_bindings_functions_round_trip() {
        let make_config = |counter: Arc<AtomicUsize>| {
            let mut bindings = storm_lua_vm::bindings::HostBindings::default();
            bindings.functions.insert(
                "ext.ping".into(),
                Arc::new(move |_| {
                    let v = counter.fetch_add(1, Ordering::Relaxed) + 1;
                    Ok(vec![LuaValue::Integer(v as i64)])
                }),
            );
            AddonConfig {
                environment: storm_lua_spec::environment::EnvironmentProfile::Extended,
                bindings,
                ..AddonConfig::default()
            }
        };
        let c1 = Arc::new(AtomicUsize::new(0));
        let mut addon = Addon::new(make_config(Arc::clone(&c1))).unwrap();
        addon
            .load(
                b"local ping = ext.ping; function onTick() g_savedata.v = ping() end",
                "ext-addon",
            )
            .unwrap();
        addon.start().unwrap();
        addon.tick(1).unwrap();
        let bytes = addon.snapshot().unwrap();
        drop(addon);

        let c2 = Arc::new(AtomicUsize::new(10));
        let mut restored = Addon::from_snapshot(make_config(Arc::clone(&c2)), &bytes).unwrap();
        restored.tick(1).unwrap();
        assert_eq!(
            restored.savedata().unwrap(),
            LuaValue::record([("v", LuaValue::Integer(11))])
        );
        assert_eq!(c1.load(Ordering::Relaxed), 1);
        assert_eq!(c2.load(Ordering::Relaxed), 11);
    }

    #[test]
    fn bug_t2_06_addon_from_snapshot_accepts_e12_sliders_and_rejects_source_stage_mismatches() {
        let mut addon = Addon::new(Default::default()).unwrap();
        addon
            .load(b"property.slider('gain', 0, 10, 1, 5)", "slider")
            .unwrap();
        let valid = addon.snapshot().unwrap();
        assert!(Addon::from_snapshot(Default::default(), &valid).is_ok());

        // Locate the slider payload: tag 1 followed by [0.0, 10.0, 1.0, 5.0] in little-endian f64.
        let mut needle = vec![1_u8];
        for v in [0.0_f64, 10.0, 1.0, 5.0] {
            needle.extend_from_slice(&v.to_le_bytes());
        }
        let pos = valid
            .windows(needle.len())
            .position(|w| w == needle.as_slice())
            .expect("slider definition present");

        // E12: the real game performs no range/step/finiteness validation on
        // property declarations, so snapshots carrying such sliders (legally
        // declared through property.slider) must restore as-is.
        for odd_slider in [
            [f64::NAN, 10.0, 1.0, 5.0],
            [10.0, 0.0, 1.0, 5.0],  // min > max
            [0.0, 10.0, 0.0, 5.0],  // increment <= 0
            [0.0, 10.0, -1.0, 5.0], // increment < 0
            [0.0, 10.0, 1.0, 20.0], // default > max
            [0.0, 10.0, 1.0, f64::INFINITY],
        ] {
            let mut altered = valid.clone();
            for (i, v) in odd_slider.into_iter().enumerate() {
                let start = pos + 1 + i * 8;
                altered[start..start + 8].copy_from_slice(&v.to_le_bytes());
            }
            let restored = Addon::from_snapshot(Default::default(), &altered)
                .unwrap_or_else(|e| panic!("E12 slider {odd_slider:?} was rejected: {e}"));
            let definitions = restored.property_definitions();
            assert_eq!(definitions.len(), 1);
            match definitions.values().next().expect("one definition") {
                MenuProperty::Slider(values) => {
                    for (actual, expected) in values.iter().zip(odd_slider.iter()) {
                        assert!(
                            actual.to_bits() == expected.to_bits(),
                            "slider value changed across restore"
                        );
                    }
                }
                other => panic!("expected a slider definition, got {other:?}"),
            }
        }

        // Stage::Empty (0) carrying non-empty loaded source must also be rejected.
        let mut bad_empty = valid.clone();
        bad_empty[MAGIC.len()] = 0; // overwrite Stage::Loaded (1) -> Stage::Empty (0)
        assert!(Addon::from_snapshot(Default::default(), &bad_empty).is_err());
    }

    #[test]
    fn bug_t2_09_addon_random_stream_continues_and_rejects_crt_ownership_mismatch() {
        struct FixedCrt;
        impl storm_lua_vm::random::CrtRandomSource for FixedCrt {
            fn draw(&self) -> Result<u32, String> {
                Ok(42)
            }
            fn seed(&self, _seed: u32) -> Result<(), String> {
                Ok(())
            }
        }

        let engine_config = AddonConfig {
            deterministic_random: true,
            ..Default::default()
        };
        let mut addon = Addon::new(engine_config.clone()).unwrap();
        addon
            .load(
                br#"
                function onTick()
                    g_savedata[1] = math.random(1, 1000000)
                    g_savedata[2] = math.random(1, 1000000)
                    g_savedata[3] = math.random(1, 1000000)
                end
            "#,
                "rng-addon",
            )
            .unwrap();
        addon.start().unwrap();
        addon.tick(1).unwrap();
        let snap = addon.snapshot().unwrap();

        addon.tick(1).unwrap();
        let expected_after_snap = addon.savedata().unwrap();

        let mut restored = Addon::from_snapshot(engine_config.clone(), &snap).unwrap();
        restored.tick(1).unwrap();
        assert_eq!(
            restored.savedata().unwrap(),
            expected_after_snap,
            "Addon Engine PRNG state was lost across snapshot/from_snapshot"
        );

        let crt_config = AddonConfig {
            random: Some(Arc::new(FixedCrt)),
            ..Default::default()
        };
        assert!(
            Addon::from_snapshot(crt_config.clone(), &snap).is_err(),
            "Engine snapshot restored into HostOwned CRT AddonConfig must fail"
        );

        let mut crt_addon = Addon::new(crt_config).unwrap();
        crt_addon
            .load(b"function onTick() g_savedata.x = math.random() end", "crt")
            .unwrap();
        crt_addon.start().unwrap();
        let crt_snap = crt_addon.snapshot().unwrap();
        assert!(
            Addon::from_snapshot(engine_config.clone(), &crt_snap).is_err(),
            "HostOwned CRT snapshot restored into Engine AddonConfig must fail"
        );
        assert!(
            Addon::from_snapshot(Default::default(), &crt_snap).is_err(),
            "HostOwned CRT snapshot restored into BuiltinCrt AddonConfig must fail"
        );

        // Tag 3 does not exist in the v2 format.
        let mut bad_tag = crt_snap;
        if let Some(last) = bad_tag.last_mut() {
            *last = 3;
        }
        assert!(Addon::from_snapshot(Default::default(), &bad_tag).is_err());
    }
}
