//! The interface every collector in this course implements.
//!
//! Workloads, tests and benchmarks are written against this trait alone, so
//! the same candidate program can be run against a reference counter, a
//! semispace copier or a generational collector without changing a line.
//!
//! Note what the trait does *not* provide. There is no `free`: from module 2
//! onwards, reclamation is the collector's job, never the mutator's. And there
//! is no way to read or write a reference slot except through [`Collector::read_field`]
//! and [`Collector::write_field`], which is what makes barriers possible.

use crate::heap::{Handle, Heap};
use crate::roots::Roots;
use crate::stats::GcStats;
use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GcError {
    /// The request could not be satisfied, even after collecting.
    OutOfMemory { requested: u32, used: u32, capacity: u32 },
}

impl fmt::Display for GcError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            GcError::OutOfMemory { requested, used, capacity } => write!(
                f,
                "out of memory: wanted {requested} bytes, {used} of {capacity} in use"
            ),
        }
    }
}

pub trait Collector {
    /// Short name used in benchmark output.
    fn name(&self) -> &'static str;

    fn heap(&self) -> &Heap;
    fn heap_mut(&mut self) -> &mut Heap;
    fn roots(&self) -> &Roots;
    fn roots_mut(&mut self) -> &mut Roots;
    fn stats(&self) -> &GcStats;

    /// Bytes currently committed to allocated objects, excluding free space.
    ///
    /// After a full collection this should equal the total size of everything
    /// still reachable. The accounting tests compare it against an independent
    /// model of the object graph.
    fn used_bytes(&self) -> u32;

    /// Allocate an object with `nrefs` reference slots and `ndata` payload
    /// bytes, collecting first if that is what it takes to find room.
    fn alloc(&mut self, nrefs: u16, ndata: u32) -> Result<Handle, GcError>;

    /// Read reference slot `i` of `obj`.
    ///
    /// Collectors that relocate objects concurrently with the mutator install
    /// a read barrier here; most collectors just read the slot.
    fn read_field(&mut self, obj: Handle, i: u16) -> Handle {
        self.heap().field(obj, i)
    }

    /// Store `val` into reference slot `i` of `obj`.
    ///
    /// This is where a write barrier lives: reference counting adjusts counts,
    /// a generational collector records old-to-young edges, and an incremental
    /// collector preserves the tri-colour invariant.
    fn write_field(&mut self, obj: Handle, i: u16, val: Handle);

    /// Run a full collection now.
    fn collect(&mut self);

    /// Called after a handle is pushed onto the shadow stack.
    fn on_root_pushed(&mut self, _h: Handle) {}

    /// Called for each handle dropped when the shadow stack unwinds.
    fn on_root_popped(&mut self, _h: Handle) {}

    /// Called when a global binding changes, with the old and new handles.
    fn on_global_changed(&mut self, _old: Handle, _new: Handle) {}
}
