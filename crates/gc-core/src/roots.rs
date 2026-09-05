//! The root set: everything a collection must treat as live by definition.
//!
//! Handles are byte offsets, so a relocating collector invalidates every one
//! it moves. Mutator code therefore never holds a `Handle` across a possible
//! collection; it holds a [`RootSlot`], which is an index into the shadow
//! stack below. The collector rewrites the handles in that stack when it moves
//! objects, so the slot keeps naming the same object forever.
//!
//! This is the trick real runtimes use: SpiderMonkey's `Rooted<T>`, LLVM's
//! `gc.root` shadow stack and Go's stack maps all exist to give the collector
//! a place to write updated addresses.

use crate::heap::{Handle, NULL};
use std::collections::HashMap;

/// A stable name for a root. Valid until the stack is unwound past it.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug, PartialOrd, Ord)]
pub struct RootSlot(pub usize);

/// The shadow stack plus a table of named globals.
#[derive(Default)]
pub struct Roots {
    stack: Vec<Handle>,
    globals: HashMap<String, Handle>,
}

impl Roots {
    pub fn new() -> Roots {
        Roots::default()
    }

    /// Push `h` onto the shadow stack and return its slot.
    pub fn push(&mut self, h: Handle) -> RootSlot {
        self.stack.push(h);
        RootSlot(self.stack.len() - 1)
    }

    /// Current handle held in `slot`.
    pub fn get(&self, slot: RootSlot) -> Handle {
        self.stack[slot.0]
    }

    /// Replace the handle held in `slot`.
    pub fn set(&mut self, slot: RootSlot, h: Handle) {
        self.stack[slot.0] = h;
    }

    /// Number of live shadow stack entries. Save this, then [`Roots::unwind`]
    /// back to it to drop every root pushed in between.
    pub fn depth(&self) -> usize {
        self.stack.len()
    }

    /// Drop shadow stack entries down to `depth`, returning what was dropped
    /// so a reference-counting collector can release them.
    pub fn unwind(&mut self, depth: usize) -> Vec<Handle> {
        self.stack.split_off(depth)
    }

    /// Bind a name to a handle in the global table.
    pub fn set_global(&mut self, name: &str, h: Handle) -> Option<Handle> {
        self.globals.insert(name.to_string(), h)
    }

    /// Look up a global, or [`NULL`] if unbound.
    pub fn global(&self, name: &str) -> Handle {
        self.globals.get(name).copied().unwrap_or(NULL)
    }

    /// Remove a global binding, returning the handle it held.
    pub fn remove_global(&mut self, name: &str) -> Option<Handle> {
        self.globals.remove(name)
    }

    pub fn global_names(&self) -> Vec<String> {
        let mut n: Vec<String> = self.globals.keys().cloned().collect();
        n.sort();
        n
    }

    /// Every non-null root, from both the shadow stack and the global table.
    pub fn iter(&self) -> impl Iterator<Item = Handle> + '_ {
        self.stack.iter().chain(self.globals.values()).copied().filter(|h| !h.is_null())
    }

    /// Visit every root slot mutably, including globals.
    ///
    /// A relocating collector must call this, or objects will still be alive
    /// but named by addresses that no longer hold them.
    pub fn for_each_mut(&mut self, mut f: impl FnMut(&mut Handle)) {
        for h in self.stack.iter_mut() {
            if !h.is_null() {
                f(h);
            }
        }
        for h in self.globals.values_mut() {
            if !h.is_null() {
                f(h);
            }
        }
    }

    /// Snapshot of the shadow stack, including nulls, for diagnostics.
    pub fn stack_snapshot(&self) -> &[Handle] {
        &self.stack
    }
}
