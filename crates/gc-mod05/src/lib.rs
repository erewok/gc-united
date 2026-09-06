//! # Module 5 — Mark-compact
//!
//! Module 4 reclaimed memory but left the heap full of holes. Module 1 showed
//! where that ends: a heap with plenty of free bytes and no single hole big
//! enough for the next object. This module fixes it permanently, by sliding the
//! survivors down against each other so that all the free space ends up in one
//! contiguous run at the top.
//!
//! What that buys is worth being precise about. Allocation becomes a bump
//! pointer — add to an integer, no free list, no search, no splitting — which
//! is the fastest allocation there is. External fragmentation stops existing:
//! if the total free space is big enough, the object fits, always. And objects
//! allocated together stay together, because sliding preserves address order,
//! which is kind to caches in a way a free list never is.
//!
//! HotSpot's serial old generation works exactly this way.
//!
//! ## Objects move
//!
//! This is the first module where they do, and it changes everything about what
//! a reference means. A [`Handle`] is a byte offset. Move the object and every
//! handle naming it is wrong — not invalid in a way that crashes, but *wrong*
//! in a way that keeps working: the address still holds a well-formed object,
//! just the wrong one.
//!
//! So a moving collector has one obligation above all others: find every
//! reference to every object it moves, and rewrite it. References live in three
//! places, and all three count.
//!
//! - reference slots inside other objects
//! - the shadow stack
//! - the global table
//!
//! [`gc_core::roots::Roots::for_each_mut`] exists for the last two.
//!
//! ## The four passes
//!
//! **Mark** — as in module 4: find what is reachable.
//!
//! **Compute forwarding addresses** — walk the heap in address order with a
//! second cursor that only advances past survivors. For each marked object,
//! record in its header where it is *going* to end up. Nothing moves yet.
//!
//! **Update references** — now that every survivor knows its destination,
//! rewrite every reference to point at the destination rather than the current
//! address. This must happen before anything moves, because moving destroys the
//! forwarding addresses as objects slide over them.
//!
//! **Move** — slide each survivor down to the address recorded for it. Order
//! matters: objects move towards lower addresses, so they must be moved from
//! the bottom up, or an object still waiting to move gets overwritten by one
//! that already has.
//!
//! ## The trap in the third pass
//!
//! An object that does not move still refers to objects that do. The first
//! objects in the heap frequently stay exactly where they are — and their
//! fields are exactly as stale as anyone else's if they are skipped.
//!
//! ## Your work
//!
//! [`MarkCompact::compute_forwarding`] and [`MarkCompact::move_objects`] are
//! unimplemented. The rest of the module is written but does not pass its
//! tests.

use std::time::Instant;

use gc_core::collector::{Collector, GcError};
use gc_core::heap::{Handle, Heap};
use gc_core::roots::Roots;
use gc_core::stats::GcStats;

/// Collect no sooner than this, so small programs pay nothing.
const MIN_THRESHOLD: u32 = 64 << 10;

/// Consecutive collections that reclaim almost nothing from a nearly full heap
/// before the collector reports the heap exhausted instead of trying again.
const THRASH_LIMIT: u32 = 4;

pub struct MarkCompact {
    heap: Heap,
    roots: Roots,
    stats: GcStats,
    worklist: Vec<Handle>,
    /// One past the last allocated byte. Everything in `[0, top)` is an object
    /// carrying a valid header, so the region can be walked.
    top: u32,
    threshold: u32,
    thrashing: u32,
}

impl MarkCompact {
    pub fn new(capacity: usize) -> MarkCompact {
        MarkCompact {
            heap: Heap::new(capacity),
            roots: Roots::new(),
            stats: GcStats::default(),
            worklist: Vec::new(),
            top: 0,
            threshold: MIN_THRESHOLD.min(capacity as u32 / 2),
            thrashing: 0,
        }
    }

    /// One past the last allocated byte.
    pub fn top(&self) -> u32 {
        self.top
    }
    pub fn is_marked(&self, h: Handle) -> bool {
        self.heap.is_marked(h)
    }
    /// Where `h` will be moved to, once forwarding addresses are computed.
    pub fn forwarding_address(&self, h: Handle) -> Handle {
        self.heap.forward(h)
    }
    /// Every object in the heap, live or dead, in address order.
    pub fn blocks(&self) -> Vec<Handle> {
        self.heap.walk(0, self.top).collect()
    }

    /// Set the mark bit on everything reachable from the roots.
    pub fn mark_from_roots(&mut self) {
        self.worklist.clear();
        self.worklist.extend(self.roots.iter());

        while let Some(h) = self.worklist.pop() {
            if self.heap.is_marked(h) {
                continue;
            }
            self.heap.set_marked(h, true);
            self.stats.objects_marked += 1;
            for child in self.heap.children(h) {
                if !self.heap.is_marked(child) {
                    self.worklist.push(child);
                }
            }
        }
    }

    /// Record, in each survivor's header, the address it will slide down to.
    ///
    /// Walk the heap in address order keeping a second cursor that starts at
    /// zero and advances only past objects that survive. Returns the address
    /// one past the last survivor: where the heap will end after compaction.
    ///
    /// Nothing is moved and no reference is changed here.
    pub fn compute_forwarding(&mut self) -> u32 {
        todo!(
            "record where each of the survivors below {} is going",
            self.top
        )
    }

    /// Rewrite every reference to point at where its target is going, rather
    /// than where it is now.
    ///
    /// Every reference counts: the ones inside surviving objects, the ones on
    /// the shadow stack, and the ones in the global table. A reference that is
    /// missed here will still address a well-formed object afterwards — just
    /// not the right one.
    pub fn update_references(&mut self) {
        let mut at = 0u32;
        while at < self.top {
            let h = Handle(at);
            at += self.heap.size(h);
            if !self.heap.is_marked(h) {
                continue;
            }
            if self.heap.forward(h).addr() == h.addr() {
                // Staying exactly where it is, so nothing about it changes.
                continue;
            }
            for i in 0..self.heap.nrefs(h) {
                let child = self.heap.field(h, i);
                if !child.is_null() {
                    let moved_to = self.heap.forward(child);
                    self.heap.set_field(h, i, moved_to);
                }
            }
        }

        let heap = &self.heap;
        self.roots.for_each_mut(|h| *h = heap.forward(*h));
    }

    /// Slide every survivor down to the address recorded in its header.
    ///
    /// Afterwards the mark bit and the forwarding address belong to a
    /// collection that is over, and the next one must not see them.
    /// Count survivors in [`gc_core::stats::GcStats::objects_copied`] and the
    /// rest in `objects_freed` and `bytes_freed`.
    pub fn move_objects(&mut self) {
        todo!("slide the survivors down to their forwarding addresses")
    }

    fn account(&mut self, h: Handle) -> Handle {
        self.stats.allocations += 1;
        self.stats.bytes_allocated += self.heap.size(h) as u64;
        self.stats.observe_used(self.top);
        h
    }
}

impl Collector for MarkCompact {
    fn name(&self) -> &'static str {
        "mark-compact"
    }

    fn heap(&self) -> &Heap {
        &self.heap
    }
    fn heap_mut(&mut self) -> &mut Heap {
        &mut self.heap
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

    /// After compaction every live byte is below the bump pointer and every
    /// free byte is above it, so this is simply where the pointer is.
    fn used_bytes(&self) -> u32 {
        self.top
    }

    fn alloc(&mut self, nrefs: u16, ndata: u32) -> Result<Handle, GcError> {
        let need = Heap::size_for(nrefs, ndata);
        let worth_collecting = self.thrashing < THRASH_LIMIT;

        if worth_collecting && self.top >= self.threshold {
            self.collect();
        }
        if self.top + need > self.heap.capacity() && worth_collecting {
            self.collect();
        }
        if self.top + need > self.heap.capacity() {
            self.stats.allocation_failures += 1;
            return Err(GcError::OutOfMemory {
                requested: need,
                used: self.top,
                capacity: self.heap.capacity(),
            });
        }

        let at = self.top;
        self.top += need;
        let h = self.heap.emplace(at, nrefs, ndata);
        Ok(self.account(h))
    }

    fn write_field(&mut self, obj: Handle, i: u16, val: Handle) {
        self.heap.set_field(obj, i, val);
    }

    fn collect(&mut self) {
        let start = Instant::now();
        let before = self.top;

        self.mark_from_roots();
        let after = self.compute_forwarding();
        self.update_references();
        self.move_objects();
        self.top = after;

        let capacity = self.heap.capacity();
        self.threshold = self.top.saturating_mul(2).max(MIN_THRESHOLD).min(capacity);
        let reclaimed = before.saturating_sub(self.top);
        if self.top > capacity / 16 * 15 && reclaimed < capacity / 32 {
            self.thrashing += 1;
        } else {
            self.thrashing = 0;
        }

        self.stats.collections += 1;
        self.stats.major_collections += 1;
        self.stats.record_pause(start.elapsed());
    }
}
