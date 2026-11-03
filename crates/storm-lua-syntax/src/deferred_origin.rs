//! Private persistent reason tails for candidate origins. Public Origin fields
//! and the JSON/binary representation stay eager and unchanged at the boundary.
//! Only the selected/observed origin materializes its complete reason array.
use crate::explanation::{OptimizationReason, ReasonOperation};
use crate::provenance::{Origin, OriginKind};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::sync::{Arc, OnceLock};

#[derive(Clone, Debug)]
struct ReasonTail {
    previous: Option<Arc<ReasonTail>>,
    reason: OptimizationReason,
    len: usize,
    repeats: usize,
}
impl Drop for ReasonTail {
    fn drop(&mut self) {
        // Long generated runs can accumulate thousands of distinct Relate
        // observations. Reclaim unshared tails iteratively on native and WASM.
        let mut previous = self.previous.take();
        while let Some(shared) = previous {
            match Arc::try_unwrap(shared) {
                Ok(mut owned) => previous = owned.previous.take(),
                Err(_) => break,
            }
        }
    }
}

/// One reason-tail node shared by many seed origins in a single batch
/// rename. Its `previous` link is empty, so it is only ever installed on
/// origins whose own tail is still empty: sharing it can never mix one
/// origin's earlier reasons into another's. Later pushes onto a sharing
/// origin go through `Arc::make_mut`, which clones the node once it is
/// shared, so sharers never observe each other's edits. The recorded
/// reason sequence expands exactly like a fresh tail node.
pub(crate) struct SharedTail(Arc<ReasonTail>);

impl SharedTail {
    pub(crate) fn new(code: &Arc<str>, operation: ReasonOperation) -> Self {
        Self(Arc::new(ReasonTail {
            previous: None,
            reason: OptimizationReason::action_shared(Arc::clone(code), operation),
            len: 1,
            repeats: 1,
        }))
    }
}

/// Interned tails for first reasons: a tail node with an empty previous
/// link is fully determined by its reason alone, so identical first reasons
/// across origins share one node instead of allocating one per origin.
/// Entries are only added while the table is small (same cap convention as
/// the other interns); beyond the cap, fresh nodes are used as before, so
/// the table never grows without bound. Later pushes onto a sharing origin
/// clone on write (`Arc::make_mut`), and observation expands tails by
/// content, so sharers never observe each other's edits and the recorded
/// reason sequence is identical to a fresh node.
const MAX_FIRST_TAIL_ENTRIES: usize = 4096;

type FirstTailIntern = std::sync::Mutex<Vec<(OptimizationReason, Arc<ReasonTail>)>>;

fn shared_first_tail(reason: &OptimizationReason) -> Arc<ReasonTail> {
    static FIRST_TAILS: std::sync::OnceLock<FirstTailIntern> = std::sync::OnceLock::new();
    let mut tails = FIRST_TAILS
        .get_or_init(|| std::sync::Mutex::new(Vec::new()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some((_, shared)) = tails.iter().find(|(key, _)| key == reason) {
        return Arc::clone(shared);
    }
    let shared = Arc::new(ReasonTail {
        previous: None,
        reason: reason.clone(),
        len: 1,
        repeats: 1,
    });
    if tails.len() < MAX_FIRST_TAIL_ENTRIES {
        tails.push((reason.clone(), Arc::clone(&shared)));
    }
    shared
}

/// Interned tails for deeper reasons: a tail node is fully determined by
/// its previous link and its reason, so origins that already share one
/// previous node and then observe the same next reason share the resulting
/// node instead of allocating one per origin. The table holds one reference
/// to every interned node, so a shared node is never mutated in place:
/// later pushes onto a sharing origin clone on write (`Arc::make_mut`),
/// and observation expands tails by content, so the recorded reason
/// sequence is identical to a fresh node. Same cap convention as the
/// first-tail table; beyond the cap, fresh nodes are used as before.
const MAX_TAIL_ENTRIES: usize = 4096;

type TailIntern =
    std::sync::Mutex<std::collections::HashMap<(usize, OptimizationReason), Arc<ReasonTail>>>;

fn shared_tail(previous: &Arc<ReasonTail>, reason: &OptimizationReason) -> Arc<ReasonTail> {
    static TAILS: std::sync::OnceLock<TailIntern> = std::sync::OnceLock::new();
    let mut tails = TAILS
        .get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let key = (Arc::as_ptr(previous) as usize, reason.clone());
    if let Some(shared) = tails.get(&key) {
        return Arc::clone(shared);
    }
    let shared = Arc::new(ReasonTail {
        previous: Some(Arc::clone(previous)),
        reason: reason.clone(),
        len: previous.len + 1,
        repeats: 1,
    });
    if tails.len() < MAX_TAIL_ENTRIES {
        tails.insert(key, Arc::clone(&shared));
    }
    shared
}

#[derive(Debug)]
pub(crate) struct StoredOrigin {
    pub value: Origin,
    retained: usize,
    tail: Option<Arc<ReasonTail>>,
    observed: OnceLock<Box<Origin>>,
}
impl Clone for StoredOrigin {
    fn clone(&self) -> Self {
        // Reuse an already observed array; otherwise keep the immutable tail.
        // Cloning a candidate never forces expansion.
        if let Some(observed) = self.observed.get() {
            return Self::from((**observed).clone());
        }
        Self {
            value: self.value.clone(),
            retained: self.retained,
            tail: self.tail.clone(),
            observed: OnceLock::new(),
        }
    }
}
impl From<Origin> for StoredOrigin {
    fn from(value: Origin) -> Self {
        Self {
            retained: value.reasons.len(),
            value,
            tail: None,
            observed: OnceLock::new(),
        }
    }
}
impl PartialEq for StoredOrigin {
    fn eq(&self, other: &Self) -> bool {
        self.get() == other.get()
    }
}
impl Eq for StoredOrigin {}
impl Serialize for StoredOrigin {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.get().serialize(serializer)
    }
}
impl<'de> Deserialize<'de> for StoredOrigin {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Origin::deserialize(deserializer).map(Self::from)
    }
}
impl StoredOrigin {
    pub fn get(&self) -> &Origin {
        if self.tail.is_none() && self.retained == self.value.reasons.len() {
            return &self.value;
        }
        self.observed.get_or_init(|| {
            let mut origin = self.value.clone();
            let tail_len = self.tail.as_ref().map_or(0, |tail| tail.len);
            let mut reasons = Vec::with_capacity(self.retained + tail_len);
            reasons.extend_from_slice(&self.value.reasons[..self.retained]);
            let mut reverse = Vec::with_capacity(tail_len.min(32));
            let mut node = self.tail.as_deref();
            while let Some(tail) = node {
                reverse.push((&tail.reason, tail.repeats));
                node = tail.previous.as_deref();
            }
            for (reason, repeats) in reverse.into_iter().rev() {
                reasons.extend(std::iter::repeat_n(reason.clone(), repeats));
            }
            origin.reasons = Arc::new(reasons);
            Box::new(origin)
        })
    }
    pub fn invalidate(&mut self) {
        self.observed.take();
    }
    pub fn last_reason(&self) -> Option<&OptimizationReason> {
        self.tail
            .as_ref()
            .map(|n| &n.reason)
            .or_else(|| self.value.reasons[..self.retained].last())
    }
    pub fn find_reason(&self, operation: ReasonOperation) -> Option<&OptimizationReason> {
        let mut next = self.tail.as_deref();
        while let Some(tail) = next {
            if tail.reason.operation == operation {
                return Some(&tail.reason);
            }
            next = tail.previous.as_deref();
        }
        self.value.reasons[..self.retained]
            .iter()
            .rev()
            .find(|r| r.operation == operation)
    }
    pub fn pop_reason(&mut self) {
        self.invalidate();
        if let Some(tail) = self.tail.as_mut().filter(|tail| tail.repeats > 1) {
            let tail = Arc::make_mut(tail);
            tail.repeats -= 1;
            tail.len -= 1;
        } else if let Some(tail) = self.tail.take() {
            self.tail = tail.previous.clone();
        } else {
            self.retained = self.retained.saturating_sub(1);
        }
    }
    fn push_reason(&mut self, reason: OptimizationReason) {
        self.invalidate();
        if let Some(tail) = self.tail.as_mut().filter(|tail| tail.reason == reason) {
            // Only the private representation is run-length encoded. Observation
            // expands the exact original sequence, including repeated actions.
            let tail = Arc::make_mut(tail);
            tail.repeats += 1;
            tail.len += 1;
        } else if let Some(previous) = self.tail.clone() {
            // Deeper reason on a non-empty tail: the resulting node is
            // fully determined by the previous node and this reason, so
            // origins sharing both share the node (see `shared_tail`).
            // The previous link is cloned, never moved, so the shared
            // node stays reachable from every origin that already held it.
            self.tail = Some(shared_tail(&previous, &reason));
        } else {
            // First reason on this origin: share the tail node (see
            // `shared_first_tail`).
            self.tail = Some(shared_first_tail(&reason));
        }
    }
    pub fn add_reason(&mut self, reason: OptimizationReason) {
        if self.last_reason() == Some(&reason) {
            return;
        }
        self.push_reason(reason);
    }
    pub fn last_operation(&mut self, operation: ReasonOperation) {
        let Some(last) = self.last_reason() else {
            return;
        };
        if last.operation == operation {
            return;
        }
        let mut replacement = last.clone();
        replacement.operation = operation;
        self.pop_reason();
        self.push_reason(replacement);
    }
    fn transform(&mut self, transformation: &str) -> Arc<str> {
        self.invalidate();
        if self.value.kind != OriginKind::Synthetic {
            self.value.kind = OriginKind::Derived;
        }
        if self.value.transformation.as_deref() != Some(transformation) {
            self.value.transformation = Some(transformation.into());
        }
        match self.value.transformation.as_ref() {
            Some(code) => Arc::clone(code),
            None => unreachable!("transformation was just assigned"),
        }
    }
    fn last_is_action(&self, code: &str, operation: ReasonOperation) -> bool {
        self.last_reason().is_some_and(|r| {
            r.code.as_ref() == code
                && r.operation == operation
                && r.before.is_none()
                && r.after.is_none()
                && r.basis.is_none()
                && r.facts.is_empty()
        })
    }
    pub fn derive_in_place(&mut self, transformation: &str) {
        let code = self.transform(transformation);
        if !self.last_is_action(transformation, ReasonOperation::Rewrite) {
            self.push_reason(OptimizationReason::action_shared(
                code,
                ReasonOperation::Rewrite,
            ));
        }
    }
    /// Record a shape-changing rewrite: append a generic Rewrite placeholder
    /// equivalent, remove that placeholder, then append the observed shape
    /// change. Do not allocate the placeholder. The code string is supplied
    /// by the caller so a batch rewrite shares one allocation.
    pub fn rewrite_in_place_code(&mut self, code: &Arc<str>, reason: OptimizationReason) {
        self.transform_code(code);
        if self.last_is_action(code.as_ref(), ReasonOperation::Rewrite) {
            self.pop_reason();
        }
        self.add_reason(reason);
    }
    pub fn derive_action(&mut self, transformation: &str, operation: ReasonOperation) {
        let code = self.transform(transformation);
        // Preserve released behavior: a pending identical Rewrite is converted,
        // whereas repeated Relate/Rename observations remain repeated records.
        if self.last_is_action(transformation, ReasonOperation::Rewrite) {
            self.last_operation(operation);
        } else {
            self.push_reason(OptimizationReason::action_shared(code, operation));
        }
    }
    /// Same as `transform`, but reuses a caller-owned code string instead of
    /// allocating a fresh `Arc<str>` per origin. The stored value is compared
    /// by string content, exactly like `transform`, so the recorded origin is
    /// identical; only the allocation is shared.
    fn transform_code(&mut self, code: &Arc<str>) -> Arc<str> {
        self.invalidate();
        if self.value.kind != OriginKind::Synthetic {
            self.value.kind = OriginKind::Derived;
        }
        if self.value.transformation.as_deref() != Some(code.as_ref()) {
            self.value.transformation = Some(Arc::clone(code));
        }
        match self.value.transformation.as_ref() {
            Some(code) => Arc::clone(code),
            None => unreachable!("transformation was just assigned"),
        }
    }
    /// Same as `derive_in_place`, reusing the caller's code string.
    pub fn derive_in_place_code(&mut self, code: &Arc<str>) {
        let code = self.transform_code(code);
        if !self.last_is_action(code.as_ref(), ReasonOperation::Rewrite) {
            self.push_reason(OptimizationReason::action_shared(
                code,
                ReasonOperation::Rewrite,
            ));
        }
    }
    /// Same as `derive_action`, reusing the caller's code string.
    pub fn derive_action_code(&mut self, code: &Arc<str>, operation: ReasonOperation) {
        let code = self.transform_code(code);
        // Preserve released behavior: a pending identical Rewrite is converted,
        // whereas repeated Relate/Rename observations remain repeated records.
        if self.last_is_action(code.as_ref(), ReasonOperation::Rewrite) {
            self.last_operation(operation);
        } else {
            self.push_reason(OptimizationReason::action_shared(code, operation));
        }
    }
    /// Same as `derive_in_place_code`, but when this origin still has an
    /// empty tail (a seed origin), the caller's shared tail node is reused
    /// instead of allocating a fresh one. Origins with a non-empty tail fall
    /// back to the ordinary push, so the recorded sequence is identical.
    pub(crate) fn derive_in_place_code_shared(&mut self, code: &Arc<str>, tail: &SharedTail) {
        let code = self.transform_code(code);
        if !self.last_is_action(code.as_ref(), ReasonOperation::Rewrite) {
            if self.tail.is_none() {
                self.tail = Some(Arc::clone(&tail.0));
            } else {
                self.push_reason(OptimizationReason::action_shared(
                    code,
                    ReasonOperation::Rewrite,
                ));
            }
        }
    }
    /// Same as `derive_action_code`, with the same sharing rule. A pending
    /// identical Rewrite in retained reasons is converted by dropping it and
    /// installing the shared node, exactly as the ordinary path drops it and
    /// pushes a fresh identical node. The caller must pass a tail built for
    /// this code and operation.
    pub(crate) fn derive_action_code_shared(
        &mut self,
        code: &Arc<str>,
        operation: ReasonOperation,
        tail: &SharedTail,
    ) {
        let code = self.transform_code(code);
        if self.last_is_action(code.as_ref(), ReasonOperation::Rewrite) {
            if self.last_reason().is_some_and(|r| r.operation == operation) {
                return;
            }
            if self.tail.is_none() {
                self.pop_reason();
                self.tail = Some(Arc::clone(&tail.0));
            } else {
                self.last_operation(operation);
            }
        } else if self.tail.is_none() {
            self.tail = Some(Arc::clone(&tail.0));
        } else {
            self.push_reason(OptimizationReason::action_shared(code, operation));
        }
    }
    pub fn derived(&self, transformation: &str) -> Self {
        let mut output = self.clone();
        output.derive_in_place(transformation);
        output
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::provenance::{OriginPrecision, SourceSpan};
    #[test]
    fn persistent_tail_matches_every_eager_action_and_observed_snapshot() {
        let initial = Origin::source(
            SourceSpan {
                source: 0,
                start: 0,
                end: 1,
            },
            OriginPrecision::Token,
            None,
        );
        let mut stored = StoredOrigin::from(initial.clone());
        let mut eager = initial;
        let mut frozen = Vec::new();
        for i in 0..400 {
            if i % 23 == 0 {
                frozen.push((stored.clone(), eager.clone()));
            }
            let rule = if i % 5 == 0 { "fold" } else { "merge" };
            eager.derive_in_place(rule);
            stored.derive_in_place(rule);
            if i % 3 == 0 {
                Arc::make_mut(&mut eager.reasons)
                    .last_mut()
                    .unwrap()
                    .operation = ReasonOperation::Relate;
                stored.last_operation(ReasonOperation::Relate);
            }
            if i % 7 == 0 {
                Arc::make_mut(&mut eager.reasons).pop();
                stored.pop_reason();
                let reason = OptimizationReason::decision(
                    "fold",
                    "known-value",
                    [("value", format!("i64:{i}"))],
                );
                eager.add_reason(reason.clone());
                stored.add_reason(reason);
            }
            assert_eq!(stored.get(), &eager);
            assert_eq!(
                serde_json::to_vec(&stored).unwrap(),
                serde_json::to_vec(&eager).unwrap()
            );
        }
        for (s, e) in frozen {
            assert_eq!(s.get(), &e);
        }
        let decoded: StoredOrigin =
            serde_json::from_slice(&serde_json::to_vec(&stored).unwrap()).unwrap();
        assert_eq!(decoded.get(), stored.get());
    }
    #[test]
    fn long_tail_is_shared_before_observation_and_dropped_without_recursive_stack() {
        let mut stored = StoredOrigin::from(Origin::synthetic("helper"));
        for _ in 0..20000 {
            stored.derive_in_place("group");
            stored.last_operation(ReasonOperation::Relate);
        }
        let clone = stored.clone();
        assert!(stored.observed.get().is_none());
        assert!(clone.observed.get().is_none());
        assert!(Arc::ptr_eq(
            stored.tail.as_ref().unwrap(),
            clone.tail.as_ref().unwrap()
        ));
        assert_eq!(clone.get().reasons.len(), 20001);
        drop(clone);
        drop(stored);
    }
    #[test]
    fn identical_first_rewrite_reasons_share_one_tail_node() {
        let code: Arc<str> = Arc::from("api-alias-optimization");
        let previous = crate::Node::Num("3".into());
        let after = crate::Node::Num("6".into());
        let reason = OptimizationReason::rewrite_shared(&code, &previous, &after);
        let mut first = StoredOrigin::from(Origin::synthetic("helper"));
        let mut second = StoredOrigin::from(Origin::synthetic("helper"));
        first.rewrite_in_place_code(&code, reason.clone());
        second.rewrite_in_place_code(&code, reason.clone());
        assert!(Arc::ptr_eq(
            first.tail.as_ref().unwrap(),
            second.tail.as_ref().unwrap()
        ));
        assert_eq!(first.get(), second.get());
        // A different second reason chains its own node after the shared
        // one without disturbing the first origin's recorded sequence.
        let other = OptimizationReason::rewrite_shared(&code, &after, &previous);
        second.rewrite_in_place_code(&code, other);
        assert!(!Arc::ptr_eq(
            first.tail.as_ref().unwrap(),
            second.tail.as_ref().unwrap()
        ));
        // The synthetic initial value already carries its own reasons, so
        // compare relatively: the second origin gained exactly one reason
        // and the first origin's sequence is its prefix.
        let first_len = first.get().reasons.len();
        assert_eq!(second.get().reasons.len(), first_len + 1);
        assert_eq!(&second.get().reasons[..first_len], &first.get().reasons[..]);
    }
    #[test]
    fn identical_second_reasons_share_one_tail_node() {
        let code: Arc<str> = Arc::from("api-alias-optimization");
        let n3 = crate::Node::Num("3".into());
        let n6 = crate::Node::Num("6".into());
        let n9 = crate::Node::Num("9".into());
        let n12 = crate::Node::Num("12".into());
        let first_reason = OptimizationReason::rewrite_shared(&code, &n3, &n6);
        let second_reason = OptimizationReason::rewrite_shared(&code, &n6, &n9);
        let third_reason = OptimizationReason::rewrite_shared(&code, &n9, &n12);
        let mut first = StoredOrigin::from(Origin::synthetic("helper"));
        let mut second = StoredOrigin::from(Origin::synthetic("helper"));
        first.rewrite_in_place_code(&code, first_reason.clone());
        second.rewrite_in_place_code(&code, first_reason.clone());
        first.rewrite_in_place_code(&code, second_reason.clone());
        second.rewrite_in_place_code(&code, second_reason.clone());
        // Both origins shared the first tail node, so the second reason
        // chains the same deeper node instead of allocating one per origin.
        assert!(Arc::ptr_eq(
            first.tail.as_ref().unwrap(),
            second.tail.as_ref().unwrap()
        ));
        assert_eq!(first.get(), second.get());
        // A further reason on one origin chains its own node without
        // disturbing the other origin's recorded sequence.
        let second_len = second.get().reasons.len();
        first.rewrite_in_place_code(&code, third_reason);
        assert!(!Arc::ptr_eq(
            first.tail.as_ref().unwrap(),
            second.tail.as_ref().unwrap()
        ));
        assert_eq!(first.get().reasons.len(), second_len + 1);
        assert_eq!(second.get().reasons.len(), second_len);
        assert_eq!(
            &first.get().reasons[..second_len],
            &second.get().reasons[..]
        );
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod direct_action_tests {
    use super::*;
    use crate::provenance::{OriginPrecision, SourceSpan};
    use crate::Node;
    #[test]
    fn direct_actions_and_compressed_runs_match_the_eager_mutation_sequence() {
        for initial in [
            Origin::source(
                SourceSpan {
                    source: 0,
                    start: 0,
                    end: 1,
                },
                OriginPrecision::Token,
                None,
            ),
            Origin::synthetic("helper"),
        ] {
            let mut eager = initial.clone();
            let mut stored = StoredOrigin::from(initial);
            for i in 0..2000 {
                let code: Arc<str> = Arc::from(if i % 17 == 0 { "fold" } else { "relate" });
                if i % 11 == 0 {
                    let previous = Node::Num("3".into());
                    let after = Node::Num("6".into());
                    let reason = OptimizationReason::rewrite_shared(&code, &previous, &after);
                    eager.derive_in_place(code.as_ref());
                    if eager
                        .reasons
                        .last()
                        .is_some_and(|r| r.code.as_ref() == code.as_ref() && r.before.is_none())
                    {
                        Arc::make_mut(&mut eager.reasons).pop();
                    }
                    eager.add_reason(reason.clone());
                    stored.rewrite_in_place_code(&code, reason);
                } else {
                    let op = if i % 13 == 0 {
                        ReasonOperation::Rename
                    } else {
                        ReasonOperation::Relate
                    };
                    eager.derive_in_place(code.as_ref());
                    Arc::make_mut(&mut eager.reasons)
                        .last_mut()
                        .unwrap()
                        .operation = op;
                    stored.derive_action(code.as_ref(), op);
                }
                if i % 101 == 0 {
                    assert_eq!(stored.get(), &eager);
                    let decoded: StoredOrigin =
                        serde_json::from_slice(&serde_json::to_vec(&stored).unwrap()).unwrap();
                    stored = decoded;
                }
            }
            assert_eq!(stored.get(), &eager);
        }
    }

    #[test]
    fn shared_tail_matches_fresh_tail_in_all_positions() {
        let code: Arc<str> = Arc::from("scope-renaming");
        let rewrite_tail = SharedTail::new(&code, ReasonOperation::Rewrite);
        let rename_tail = SharedTail::new(&code, ReasonOperation::Rename);
        let seed = || {
            StoredOrigin::from(Origin::source(
                SourceSpan {
                    source: 0,
                    start: 0,
                    end: 1,
                },
                OriginPrecision::Token,
                None,
            ))
        };
        // Seed origins: shared install expands exactly like a fresh tail.
        let mut shared = seed();
        let mut fresh = seed();
        shared.derive_in_place_code_shared(&code, &rewrite_tail);
        fresh.derive_in_place_code(&code);
        assert_eq!(shared.get(), fresh.get());
        let mut shared = seed();
        let mut fresh = seed();
        shared.derive_action_code_shared(&code, ReasonOperation::Rename, &rename_tail);
        fresh.derive_action_code(&code, ReasonOperation::Rename);
        assert_eq!(shared.get(), fresh.get());
        // Origins with a non-empty tail fall back to ordinary pushes, and a
        // second identical observation run-length encodes through the
        // shared node without leaking into other sharers.
        let mut shared = seed();
        let mut fresh = seed();
        shared.derive_action_code(&code, ReasonOperation::Rename);
        fresh.derive_action_code(&code, ReasonOperation::Rename);
        shared.derive_action_code_shared(&code, ReasonOperation::Rename, &rename_tail);
        fresh.derive_action_code(&code, ReasonOperation::Rename);
        assert_eq!(shared.get(), fresh.get());
        // A different origin sharing the same tail node still expands to a
        // single observation: the repeat above did not leak into it.
        let mut other = seed();
        other.derive_action_code_shared(&code, ReasonOperation::Rename, &rename_tail);
        let mut single = seed();
        single.derive_action_code(&code, ReasonOperation::Rename);
        assert_eq!(other.get(), single.get());
        // A pending identical Rewrite in retained reasons is converted by
        // dropping it and installing the shared Rename node.
        let retained_rewrite = || {
            let mut origin = Origin::source(
                SourceSpan {
                    source: 0,
                    start: 0,
                    end: 1,
                },
                OriginPrecision::Token,
                None,
            );
            origin.reasons = Arc::new(vec![OptimizationReason::action(
                "scope-renaming",
                ReasonOperation::Rewrite,
            )]);
            StoredOrigin::from(origin)
        };
        let mut shared = retained_rewrite();
        let mut fresh = retained_rewrite();
        shared.derive_action_code_shared(&code, ReasonOperation::Rename, &rename_tail);
        fresh.derive_action_code(&code, ReasonOperation::Rename);
        assert_eq!(shared.get(), fresh.get());
    }
}
