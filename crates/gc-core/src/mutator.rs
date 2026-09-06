//! The mutator: the program whose garbage is being collected.
//!
//! Workloads are written against this API rather than against the collector
//! directly, for two reasons.
//!
//! First, it enforces the shadow-stack discipline. Every allocation returns a
//! [`RootSlot`], not a `Handle`, so workload code physically cannot hold a raw
//! address across a collection that might move it.
//!
//! Second, it maintains the shadow [`Model`] of the object graph in lockstep
//! with the real heap. That model is what the tests check the collector
//! against, and the mutator is the only thing that touches both.

use crate::collector::{Collector, GcError};
use crate::heap::{Handle, NULL};
use crate::model::{ID_OFF, MAGIC, MAGIC_OFF, Model, STAMP_BYTES};
use crate::roots::RootSlot;
use crate::stats::GcStats;
use crate::verify::{self, Report};

pub struct Mutator<C: Collector> {
    /// The collector under test. Module-specific tests reach through this.
    pub gc: C,
    model: Model,
}

impl<C: Collector> Mutator<C> {
    pub fn new(gc: C) -> Mutator<C> {
        Mutator {
            gc,
            model: Model::new(),
        }
    }

    /// Allocate an object with `nrefs` reference slots and `extra` bytes of
    /// scalar payload, root it, and return its slot.
    ///
    /// Panics if the heap cannot satisfy the request; use [`Mutator::try_alloc`]
    /// when running out of memory is the thing being tested.
    pub fn alloc(&mut self, nrefs: u16, extra: u32) -> RootSlot {
        match self.try_alloc(nrefs, extra) {
            Ok(s) => s,
            Err(e) => panic!("allocation of {nrefs} refs + {extra} data bytes failed: {e}"),
        }
    }

    pub fn try_alloc(&mut self, nrefs: u16, extra: u32) -> Result<RootSlot, GcError> {
        let h = self.gc.alloc(nrefs, STAMP_BYTES + extra)?;
        let id = self.model.alloc(nrefs, extra);
        self.gc.heap_mut().set_data_u64(h, ID_OFF, id);
        self.gc.heap_mut().set_data_u32(h, MAGIC_OFF, MAGIC);
        let slot = self.gc.roots_mut().push(h);
        self.gc.on_root_pushed(h);
        self.model.push_root(Some(id));
        Ok(slot)
    }

    /// Store the object in `child` into reference slot `i` of `parent`.
    pub fn store(&mut self, parent: RootSlot, i: u16, child: RootSlot) {
        let (p, c) = (self.handle(parent), self.handle(child));
        self.gc.write_field(p, i, c);
        let (pid, cid) = (self.id(parent), self.id(child));
        self.model
            .store(pid.expect("store into a null slot"), i, cid);
    }

    /// Null out reference slot `i` of `parent`.
    pub fn store_null(&mut self, parent: RootSlot, i: u16) {
        let p = self.handle(parent);
        self.gc.write_field(p, i, NULL);
        let pid = self.id(parent).expect("store into a null slot");
        self.model.store(pid, i, None);
    }

    /// Read reference slot `i` of `parent` and root the result.
    pub fn load(&mut self, parent: RootSlot, i: u16) -> RootSlot {
        let p = self.handle(parent);
        let child = self.gc.read_field(p, i);
        let pid = self.id(parent).expect("load from a null slot");
        let cid = self.model.field(pid, i);
        let slot = self.gc.roots_mut().push(child);
        if !child.is_null() {
            self.gc.on_root_pushed(child);
        }
        self.model.push_root(cid);
        slot
    }

    /// Root the same object a second time, as a fresh local variable.
    pub fn dup(&mut self, slot: RootSlot) -> RootSlot {
        let h = self.handle(slot);
        let id = self.id(slot);
        let new = self.gc.roots_mut().push(h);
        if !h.is_null() {
            self.gc.on_root_pushed(h);
        }
        self.model.push_root(id);
        new
    }

    /// Assign the object in `src` to the local variable `dst`.
    ///
    /// The new value is retained before the old one is released, which matters
    /// enormously when they are the same object.
    pub fn assign(&mut self, dst: RootSlot, src: RootSlot) {
        let new = self.handle(src);
        let old = self.handle(dst);
        if !new.is_null() {
            self.gc.on_root_pushed(new);
        }
        self.gc.roots_mut().set(dst, new);
        if !old.is_null() {
            self.gc.on_root_popped(old);
        }
        let id = self.id(src);
        self.model.set_root(dst.0, id);
    }

    /// Current address of the object in `slot`.
    pub fn handle(&self, slot: RootSlot) -> Handle {
        self.gc.roots().get(slot)
    }

    /// Model identity of the object in `slot`.
    pub fn id(&self, slot: RootSlot) -> Option<u64> {
        self.model.root(slot.0)
    }

    pub fn is_null(&self, slot: RootSlot) -> bool {
        self.model.root(slot.0).is_none()
    }

    /// Current shadow stack depth. Pass this to [`Mutator::unwind`] later to
    /// drop every root created in between.
    pub fn root_depth(&self) -> usize {
        self.gc.roots().depth()
    }

    pub fn unwind(&mut self, depth: usize) {
        let dropped = self.gc.roots_mut().unwind(depth);
        for h in dropped {
            if !h.is_null() {
                self.gc.on_root_popped(h);
            }
        }
        self.model.unwind(depth);
    }

    pub fn set_global(&mut self, name: &str, slot: RootSlot) {
        let h = self.handle(slot);
        let old = self.gc.roots_mut().set_global(name, h).unwrap_or(NULL);
        self.gc.on_global_changed(old, h);
        match self.id(slot) {
            Some(id) => self.model.set_global(name, id),
            None => self.model.remove_global(name),
        }
    }

    /// Root the object currently bound to `name`.
    pub fn global(&mut self, name: &str) -> RootSlot {
        let h = self.gc.roots().global(name);
        let id = self.model.global(name);
        let slot = self.gc.roots_mut().push(h);
        if !h.is_null() {
            self.gc.on_root_pushed(h);
        }
        self.model.push_root(id);
        slot
    }

    pub fn clear_global(&mut self, name: &str) {
        let old = self.gc.roots_mut().remove_global(name).unwrap_or(NULL);
        self.gc.on_global_changed(old, NULL);
        self.model.remove_global(name);
    }

    /// Write scalar word `index` of the object's user payload.
    pub fn write_u64(&mut self, slot: RootSlot, index: u32, v: u64) {
        let h = self.handle(slot);
        self.gc
            .heap_mut()
            .set_data_u64(h, STAMP_BYTES + 8 * index, v);
    }

    /// Read scalar word `index` of the object's user payload.
    pub fn read_u64(&mut self, slot: RootSlot, index: u32) -> u64 {
        let h = self.handle(slot);
        self.gc.heap().data_u64(h, STAMP_BYTES + 8 * index)
    }

    pub fn collect(&mut self) {
        self.gc.collect();
    }

    pub fn stats(&self) -> &GcStats {
        self.gc.stats()
    }

    pub fn used_bytes(&self) -> u32 {
        self.gc.used_bytes()
    }

    pub fn model(&self) -> &Model {
        &self.model
    }

    /// Discard model entries for objects that are provably unreachable, so a
    /// long-running workload does not grow the model without bound.
    ///
    /// Only safe once the collector has had a chance to reclaim them.
    pub fn forget_unreachable(&mut self) {
        self.model.forget_unreachable();
    }

    /// Walk the heap and compare it against the model.
    pub fn verify(&mut self) -> Report {
        verify::verify(&mut self.gc, &self.model)
    }

    /// Panic unless the heap agrees with the model.
    #[track_caller]
    pub fn assert_consistent(&mut self) {
        let report = self.verify();
        assert!(report.ok(), "{report}");
    }

    /// Panic unless the heap agrees with the model *and* nothing unreachable
    /// is still occupying space.
    #[track_caller]
    pub fn assert_fully_reclaimed(&mut self) {
        self.assert_consistent();
        if let Some(v) = verify::check_accounting(&self.gc, &self.model) {
            panic!("{v}");
        }
    }
}
