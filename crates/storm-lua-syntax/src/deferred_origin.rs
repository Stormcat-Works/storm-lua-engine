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
        } else {
            let len = self.tail.as_ref().map_or(1, |tail| tail.len + 1);
            self.tail = Some(Arc::new(ReasonTail {
                previous: self.tail.take(),
                reason,
                len,
                repeats: 1,
            }));
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
    pub fn rewrite_in_place(&mut self, transformation: &str, reason: OptimizationReason) {
        self.transform(transformation);
        // Equivalent to append generic Rewrite, remove that placeholder, then
        // append the observed shape change. Do not allocate the placeholder.
        if self.last_is_action(transformation, ReasonOperation::Rewrite) {
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
                let code = if i % 17 == 0 { "fold" } else { "relate" };
                if i % 11 == 0 {
                    let previous = Node::Num("3".into());
                    let after = Node::Num("6".into());
                    let reason = OptimizationReason::rewrite(code, &previous, &after);
                    eager.derive_in_place(code);
                    if eager
                        .reasons
                        .last()
                        .is_some_and(|r| r.code.as_ref() == code && r.before.is_none())
                    {
                        Arc::make_mut(&mut eager.reasons).pop();
                    }
                    eager.add_reason(reason.clone());
                    stored.rewrite_in_place(code, reason);
                } else {
                    let op = if i % 13 == 0 {
                        ReasonOperation::Rename
                    } else {
                        ReasonOperation::Relate
                    };
                    eager.derive_in_place(code);
                    Arc::make_mut(&mut eager.reasons)
                        .last_mut()
                        .unwrap()
                        .operation = op;
                    stored.derive_action(code, op);
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
}
