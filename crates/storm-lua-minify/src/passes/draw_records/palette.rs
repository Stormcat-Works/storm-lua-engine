//! Exact scalar dictionaries for sparse/repeated argument values.
//! Original literal nodes, including float subtype, negative zero, nil and
//! strings, live in private lookup tables. Only their integer indices are packed.
use super::*;

pub(super) fn proposals(
    shape: &Shape,
    targets: &[NodeId],
    columns: &[Vec<Atom>],
    ints: &[Option<Vec<i64>>],
    lens: &[usize],
    count: usize,
) -> Vec<(Shape, Vec<NodeId>, Payload)> {
    let mut indexed = columns.to_vec();
    let mut indexed_ints = Vec::with_capacity(columns.len());
    let mut indexed_lens = Vec::with_capacity(columns.len());
    let mut mappings = Vec::with_capacity(columns.len());
    let mut changed = false;
    // Index atoms repeat across every element (at most 17 distinct
    // values), so spell each one once per call and clone the shared atom
    // instead of allocating a fresh spelling per element.
    let mut index_atoms: Vec<Atom> = Vec::new();
    for (i, column) in columns.iter().enumerate() {
        let mut values = Vec::new();
        let mut indices = Vec::with_capacity(count);
        let mut indices_ints = Vec::with_capacity(count);
        let mut seen = BTreeMap::new();
        for atom in column {
            let next = values.len() + 1;
            let index = *seen.entry(atom.clone()).or_insert_with(|| {
                values.push(atom.clone());
                next
            });
            while index_atoms.len() < index {
                let spelling = index_atoms.len() + 1;
                index_atoms.push(Atom::Num(spelling.to_string().into()));
            }
            indices.push(index_atoms[index - 1].clone());
            indices_ints.push(index as i64);
            if values.len() > 16 {
                break;
            }
        }
        let compact_range = ints[i].as_ref().is_some_and(|v| {
            #[expect(
                clippy::unwrap_used,
                reason = "Palette candidates are built from nonempty draw-call columns"
            )]
            let low = *v.iter().min().unwrap();
            #[expect(
                clippy::unwrap_used,
                reason = "Palette candidates are built from nonempty draw-call columns"
            )]
            let high = *v.iter().max().unwrap();
            (high as i128 - low as i128) < values.len() as i128 * 2
        });
        if values.len() < 2 || values.len() > 16 || count < values.len() * 3 || compact_range {
            mappings.push(None);
            indexed_ints.push(ints[i].clone());
            indexed_lens.push(lens[i]);
        } else {
            indexed_lens.push(indices.iter().map(|a| atom_len(a, 0)).sum());
            indexed[i] = indices;
            indexed_ints.push(Some(indices_ints));
            mappings.push(Some(values));
            changed = true;
        }
    }
    if !changed {
        return Vec::new();
    }
    let mut mapped = shape.clone();
    for arg in mapped.args.iter_mut().flatten() {
        if let Arg::Column(i) = arg {
            if let Some(values) = &mappings[*i] {
                *arg = Arg::Lookup(*i, values.clone());
            }
        }
    }
    // encode_columns does not call this projection, so recursion is bounded.
    encode_columns(
        mapped,
        targets.to_vec(),
        indexed,
        indexed_ints,
        &indexed_lens,
        count,
        true,
        true,
    )
}
