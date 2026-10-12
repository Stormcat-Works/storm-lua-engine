//! Private paged copy-on-write origin slots. Small candidate edits detach only
//! their page; serialization remains the released flat array of node origins.
use crate::provenance::NodeOrigin;
use serde::ser::SerializeSeq;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::ops::{Index, IndexMut};
use std::sync::Arc;

type Slot = Option<Arc<NodeOrigin>>;
const PAGE: usize = 64;
#[derive(Debug, Clone, Default)]
pub(crate) struct OriginSlots {
    pages: Vec<Arc<Vec<Slot>>>,
    len: usize,
}
impl OriginSlots {
    pub fn len(&self) -> usize {
        self.len
    }
    pub fn get(&self, index: usize) -> Option<&Slot> {
        if index >= self.len {
            return None;
        }
        self.pages.get(index / PAGE)?.get(index % PAGE)
    }
    pub fn iter(&self) -> impl Iterator<Item = &Slot> {
        self.pages.iter().flat_map(|page| page.iter())
    }
    pub fn push(&mut self, value: Slot) {
        if self.len.is_multiple_of(PAGE) {
            self.pages.push(Arc::new(Vec::with_capacity(PAGE)));
        }
        let page_index = self.len / PAGE;
        Arc::make_mut(&mut self.pages[page_index]).push(value);
        self.len += 1;
    }
    pub fn truncate(&mut self, len: usize) {
        if len >= self.len {
            return;
        }
        self.pages.truncate(len.div_ceil(PAGE));
        let remainder = len % PAGE;
        if remainder > 0 {
            let page_index = len / PAGE;
            Arc::make_mut(&mut self.pages[page_index]).truncate(remainder);
        }
        self.len = len;
    }
    pub fn fill(&mut self, value: Slot) {
        for page in &mut self.pages {
            Arc::make_mut(page).fill(value.clone());
        }
    }
}
impl From<Vec<Slot>> for OriginSlots {
    fn from(values: Vec<Slot>) -> Self {
        let len = values.len();
        let mut source = values.into_iter();
        let mut pages = Vec::with_capacity(len.div_ceil(PAGE));
        while !source.as_slice().is_empty() {
            pages.push(Arc::new(source.by_ref().take(PAGE).collect()));
        }
        Self { pages, len }
    }
}
impl Index<usize> for OriginSlots {
    type Output = Slot;
    fn index(&self, index: usize) -> &Slot {
        &self.pages[index / PAGE][index % PAGE]
    }
}
impl IndexMut<usize> for OriginSlots {
    fn index_mut(&mut self, index: usize) -> &mut Slot {
        &mut Arc::make_mut(&mut self.pages[index / PAGE])[index % PAGE]
    }
}
impl Serialize for OriginSlots {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut seq = s.serialize_seq(Some(self.len))?;
        for item in self.iter() {
            seq.serialize_element(item)?;
        }
        seq.end()
    }
}
impl<'de> Deserialize<'de> for OriginSlots {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Vec::<Slot>::deserialize(d).map(Self::from)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::provenance::Origin;
    fn slot(n: usize) -> Slot {
        Some(Arc::new(NodeOrigin {
            origin: Some(Origin::synthetic(&format!("slot-{n}")).into()),
            ..NodeOrigin::default()
        }))
    }
    #[test]
    fn page_boundaries_push_reuse_and_flat_wire_match_the_previous_vector() {
        for size in [0, 1, 63, 64, 65, 127, 128, 129, 4097] {
            let original = (0..size).map(slot).collect::<Vec<_>>();
            let mut storage = OriginSlots::from(original.clone());
            assert_eq!(storage.len(), original.len());
            assert_eq!(
                serde_json::to_vec(&storage).unwrap(),
                serde_json::to_vec(&original).unwrap()
            );
            for count in [size / 2, 0, size] {
                let mut expected = original.clone();
                storage = OriginSlots::from(original.clone());
                expected.truncate(count);
                storage.truncate(count);
                for n in 0..130 {
                    expected.push(slot(n));
                    storage.push(slot(n));
                }
                assert_eq!(
                    serde_json::to_vec(&storage).unwrap(),
                    serde_json::to_vec(&expected).unwrap()
                );
                assert!(storage.get(storage.len()).is_none());
                let decoded: OriginSlots =
                    serde_json::from_slice(&serde_json::to_vec(&expected).unwrap()).unwrap();
                assert_eq!(
                    serde_json::to_vec(&decoded).unwrap(),
                    serde_json::to_vec(&expected).unwrap()
                );
                storage.fill(None);
                expected.fill(None);
                assert_eq!(
                    serde_json::to_vec(&storage).unwrap(),
                    serde_json::to_vec(&expected).unwrap()
                );
            }
        }
    }
    #[test]
    fn a_single_edit_preserves_unmodified_pages_and_the_original_snapshot() {
        let original = OriginSlots::from((0..4096).map(slot).collect::<Vec<_>>());
        let bytes = serde_json::to_vec(&original).unwrap();
        let mut changed = original.clone();
        changed[129] = None;
        assert!(original[129].is_some());
        assert!(changed[129].is_none());
        for index in 0..original.pages.len() {
            assert_eq!(
                Arc::ptr_eq(&original.pages[index], &changed.pages[index]),
                index != 129 / PAGE
            );
        }
        assert_eq!(serde_json::to_vec(&original).unwrap(), bytes);
        let mut truncated = original.clone();
        truncated.truncate(130);
        truncated.push(None);
        assert!(Arc::ptr_eq(&truncated.pages[0], &original.pages[0]));
        assert!(original[130].is_some());
        assert!(truncated[130].is_none());
    }
}
