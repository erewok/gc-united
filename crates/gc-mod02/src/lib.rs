//! # Module 2 — Reference counting
//!
//! The first actual reclamation strategy: every object records how many
//! references point at it, and when that count reaches zero the object is
//! dead, immediately and by definition.
//!
//! Reference counting has properties nothing else in this course has. Memory is
//! reclaimed the instant it becomes unreachable, so the heap stays small and a
//! destructor could run at a predictable moment. There is no tracing phase and
//! therefore no pause proportional to the size of the live set. CPython, Swift,
//! Objective-C's ARC, `std::shared_ptr` and Rust's own `Rc` all work this way.
//!
//! The costs are just as characteristic. Every reference assignment becomes a
//! read, two arithmetic updates and a branch, so the mutator pays continuously
//! rather than in occasional pauses. Dropping the last reference to the root of
//! a large structure frees the whole thing at once, which is a pause by another
//! name. And, fatally, a group of objects that reference each other keeps its
//! own counts above zero forever. Module 3 exists to fix that.
//!
//! ## Where counts change
//!
//! A count changes at exactly four moments, and missing any one of them is
//! either a leak or a crash:
//!
//! - a reference is pushed onto the shadow stack, or popped off it
//! - a reference is stored into an object's field, replacing what was there
//! - a global binding is rebound, replacing what it named
//! - an object dies, releasing everything it referred to
//!
//! The last is the interesting one. Freeing an object is not one operation but
//! two: the object's own memory goes back to the allocator, *and* every
//! reference it held has to be released, which may kill more objects, which may
//! kill more again. Doing that with native recursion would blow the stack on a
//! long list, so [`RefCount::release`] carries an explicit worklist.
//!
//! ## The ordering that matters
//!
//! When a reference is overwritten, the new value is retained before the old
//! one is released. The two orders differ only when the new and old values are
//! the same object — `x.field = x.field`, which real programs and real code
//! generators produce constantly. Releasing first takes that object's count to
//! zero and frees it, and the retain that follows resurrects a corpse.
//!
//! ## Your work
//!
//! [`RefCount::on_global_changed`] is unimplemented; the shadow stack hooks
//! next to it show the shape. The rest of the module is written but does not
//! pass its tests.

use std::time::Instant;

use gc_core::collector::{Collector, GcError};
use gc_core::heap::{Handle, Heap};
use gc_core::roots::Roots;
use gc_core::space::FreeListSpace;
use gc_core::stats::GcStats;

pub struct RefCount {
    space: FreeListSpace,
    roots: Roots,
    stats: GcStats,
    /// Objects whose count still has to be decremented. Held here rather than
    /// on the native stack so that freeing a long chain cannot overflow it.
    pending: Vec<Handle>,
}

impl RefCount {
    pub fn new(capacity: usize) -> RefCount {
        RefCount {
            space: FreeListSpace::new(capacity),
            roots: Roots::new(),
            stats: GcStats::default(),
            pending: Vec::new(),
        }
    }

    /// The current reference count of `h`.
    pub fn rc_of(&self, h: Handle) -> u32 {
        self.space.heap().rc(h)
    }

    /// Whether `h`'s memory has been returned to the allocator.
    pub fn is_freed(&self, h: Handle) -> bool {
        self.space.is_free(h)
    }

    pub fn space(&self) -> &FreeListSpace {
        &self.space
    }
    pub fn space_mut(&mut self) -> &mut FreeListSpace {
        &mut self.space
    }

    /// Record one more reference to `h`.
    pub fn retain(&mut self, h: Handle) {
        if h.is_null() {
            return;
        }
        let rc = self.space.heap().rc(h);
        self.space.heap_mut().set_rc(h, rc + 1);
    }

    /// Record one fewer reference to `h`, freeing it if that was the last one.
    ///
    /// Freeing an object releases everything it referred to, which can cascade
    /// through an arbitrarily long chain, so the cascade runs on an explicit
    /// worklist rather than by recursion.
    pub fn release(&mut self, h: Handle) {
        if h.is_null() {
            return;
        }
        self.pending.push(h);

        while let Some(obj) = self.pending.pop() {
            let rc = self.space.heap().rc(obj);
            assert!(
                rc > 0,
                "{obj:?} was released while its reference count was already zero"
            );
            self.space.heap_mut().set_rc(obj, rc - 1);

            if rc == 1 {
                let children = self.space.heap().children(obj);
                self.pending.extend(children);
                // this release has to go last also
                self.free_object(obj);
            }
        }
    }

    fn free_object(&mut self, h: Handle) {
        let size = self.space.heap().size(h);
        self.space.free(h);
        self.stats.objects_freed += 1;
        self.stats.bytes_freed += size as u64;
    }
}

impl Collector for RefCount {
    fn name(&self) -> &'static str {
        "reference counting"
    }

    fn heap(&self) -> &Heap {
        self.space.heap()
    }
    fn heap_mut(&mut self) -> &mut Heap {
        self.space.heap_mut()
    }
    fn roots(&self) -> &Roots {
        &self.roots
    }
    fn roots_mut(&mut self) -> &mut Roots {
        &mut self.roots
    }
    fn stats(&self) -> &GcStats {
        &self.stats
    }
    fn used_bytes(&self) -> u32 {
        self.space.used_bytes()
    }

    fn alloc(&mut self, nrefs: u16, ndata: u32) -> Result<Handle, GcError> {
        let h = match self.space.alloc(nrefs, ndata) {
            Some(h) => h,
            None => {
                // Nothing can be reclaimed by collecting — counts are already
                // exact — but adjacent holes may add up to enough room.
                self.space.coalesce();
                match self.space.alloc(nrefs, ndata) {
                    Some(h) => h,
                    None => {
                        self.stats.allocation_failures += 1;
                        return Err(GcError::OutOfMemory {
                            requested: Heap::size_for(nrefs, ndata),
                            used: self.space.used_bytes(),
                            capacity: self.space.capacity(),
                        });
                    }
                }
            }
        };
        self.stats.allocations += 1;
        self.stats.bytes_allocated += self.space.heap().size(h) as u64;
        self.stats.observe_used(self.space.used_bytes());
        Ok(h)
    }

    /// Store `val` into reference slot `i` of `obj`, keeping counts exact.
    ///
    /// `obj` gains a reference to `val` and loses its reference to whatever the
    /// slot held before. Both counts have to move.
    fn write_field(&mut self, obj: Handle, i: u16, val: Handle) {
        self.retain(val);
        // set obj.i => val reference here and lose obj.i => old
        let old = self.heap().field(obj, i);
        self.heap_mut().set_field(obj, i, val);
        // if last reference, the val could may have been accidentally gc'd, so we decrement last
        self.release(old);
    }

    /// Rebind a global from `old` to `new`.
    ///
    /// The global table is part of the root set, so a binding is a reference
    /// like any other: rebinding one both creates a reference and destroys one.
    fn on_global_changed(&mut self, old: Handle, new: Handle) {
        self.on_root_pushed(new);
        self.on_root_popped(old);
    }

    fn on_root_pushed(&mut self, h: Handle) {
        self.retain(h);
    }

    fn on_root_popped(&mut self, h: Handle) {
        self.release(h);
    }

    /// Reference counting has no collection phase: an object is freed the
    /// moment its last reference goes away. All this can usefully do is merge
    /// adjacent holes so that a large request is not refused by a heap that
    /// has plenty of free bytes in pieces.
    fn collect(&mut self) {
        let start = Instant::now();
        self.space.coalesce();
        self.stats.collections += 1;
        self.stats.record_pause(start.elapsed());
    }
}
