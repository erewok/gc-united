//! An independent model of the object graph the mutator built.
//!
//! The model is maintained by [`crate::mutator::Mutator`] in lockstep with the
//! real heap, and it is deliberately trivial: a map from object identity to
//! its fields, plus a mirror of the root set. It never consults the collector,
//! so it stays correct no matter how badly the collector misbehaves.
//!
//! Identity is a `u64` stamped into every object's payload at allocation, next
//! to a magic number. That stamp is what lets verification tell "this handle
//! points at the object I expect" apart from "this handle points at some other
//! object that happens to live here now".

use std::collections::{BTreeSet, HashMap};

use crate::heap::Heap;

/// Payload offset of the identity stamp.
pub const ID_OFF: u32 = 0;
/// Payload offset of the magic number.
pub const MAGIC_OFF: u32 = 8;
/// Value written at [`MAGIC_OFF`].
pub const MAGIC: u32 = 0xC0FF_EE01;
/// Payload bytes reserved for the stamp. User data starts after this.
pub const STAMP_BYTES: u32 = 12;

#[derive(Clone, Debug)]
pub struct ModelObj {
    pub id: u64,
    pub nrefs: u16,
    /// Payload bytes beyond the stamp.
    pub extra: u32,
    /// Total heap footprint including header and padding.
    pub size: u32,
    /// Reference slots, by identity. `None` is a null slot.
    pub fields: Vec<Option<u64>>,
    /// Allocation order, for readable diagnostics.
    pub seq: u64,
}

#[derive(Default)]
pub struct Model {
    objs: HashMap<u64, ModelObj>,
    stack: Vec<Option<u64>>,
    globals: HashMap<String, u64>,
    next_id: u64,
    seq: u64,
}

impl Model {
    pub fn new() -> Model {
        Model {
            next_id: 1,
            ..Model::default()
        }
    }

    /// Record a new object and return its identity.
    pub fn alloc(&mut self, nrefs: u16, extra: u32) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        self.seq += 1;
        self.objs.insert(
            id,
            ModelObj {
                id,
                nrefs,
                extra,
                size: Heap::size_for(nrefs, STAMP_BYTES + extra),
                fields: vec![None; nrefs as usize],
                seq: self.seq,
            },
        );
        id
    }

    pub fn get(&self, id: u64) -> Option<&ModelObj> {
        self.objs.get(&id)
    }

    pub fn store(&mut self, parent: u64, i: u16, child: Option<u64>) {
        let p = self
            .objs
            .get_mut(&parent)
            .expect("model: store into unknown object");
        p.fields[i as usize] = child;
    }

    pub fn field(&self, parent: u64, i: u16) -> Option<u64> {
        self.objs[&parent].fields[i as usize]
    }

    pub fn push_root(&mut self, id: Option<u64>) {
        self.stack.push(id);
    }

    pub fn unwind(&mut self, depth: usize) {
        self.stack.truncate(depth);
    }

    pub fn root_depth(&self) -> usize {
        self.stack.len()
    }

    pub fn root(&self, slot: usize) -> Option<u64> {
        self.stack[slot]
    }

    pub fn set_root(&mut self, slot: usize, id: Option<u64>) {
        self.stack[slot] = id;
    }

    pub fn set_global(&mut self, name: &str, id: u64) {
        self.globals.insert(name.to_string(), id);
    }

    pub fn remove_global(&mut self, name: &str) {
        self.globals.remove(name);
    }

    pub fn global(&self, name: &str) -> Option<u64> {
        self.globals.get(name).copied()
    }

    pub fn globals(&self) -> impl Iterator<Item = (&String, &u64)> {
        self.globals.iter()
    }

    /// Identities named directly by a root.
    pub fn root_ids(&self) -> Vec<u64> {
        self.stack
            .iter()
            .flatten()
            .copied()
            .chain(self.globals.values().copied())
            .collect()
    }

    /// Every identity transitively reachable from a root. This is the exact
    /// set a precise collector must keep, and nothing more.
    pub fn reachable(&self) -> BTreeSet<u64> {
        let mut seen = BTreeSet::new();
        let mut work: Vec<u64> = self.root_ids();
        while let Some(id) = work.pop() {
            if !seen.insert(id) {
                continue;
            }
            if let Some(o) = self.objs.get(&id) {
                work.extend(o.fields.iter().flatten().copied());
            }
        }
        seen
    }

    /// Total heap footprint of everything reachable.
    pub fn reachable_bytes(&self) -> u64 {
        self.reachable()
            .iter()
            .filter_map(|id| self.objs.get(id))
            .map(|o| o.size as u64)
            .sum()
    }

    /// Every object ever allocated and not yet forgotten by the model.
    pub fn all_ids(&self) -> BTreeSet<u64> {
        self.objs.keys().copied().collect()
    }

    /// Drop objects the model can prove are unreachable, so that long
    /// workloads do not grow the model without bound.
    pub fn forget_unreachable(&mut self) {
        let live = self.reachable();
        self.objs.retain(|id, _| live.contains(id));
    }

    pub fn len(&self) -> usize {
        self.objs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.objs.is_empty()
    }
}
