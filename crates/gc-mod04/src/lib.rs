//! # Module 4 — Mark and sweep
//!
//! The first *tracing* collector, and the point at which the course changes
//! its mind about what "garbage" means.
//!
//! Modules 2 and 3 asked each object how many references pointed at it. This
//! module never asks. It starts from the roots — the only things reachable by
//! definition — and follows every reference it can, marking what it arrives at.
//! Whatever is left unmarked is unreachable, and unreachable is the actual
//! definition of garbage. Cycles need no special handling at all: an abandoned
//! ring is simply never reached, and the elaborate machinery of module 3
//! evaporates.
//!
//! McCarthy described this in 1960 for Lisp, and it is still the backbone of
//! most collectors in production. Boehm's conservative collector for C and
//! C++, Ruby's collector before it grew generations, and the mature space of
//! most generational collectors are all mark-sweep at heart.
//!
//! ## The two phases
//!
//! **Mark** walks the reachable graph from the roots and sets a bit on every
//! object it reaches. The cost is proportional to the amount of *live* data.
//!
//! **Sweep** walks the heap linearly from bottom to top, and every object whose
//! bit is clear is garbage and goes back to the allocator. The cost is
//! proportional to the size of the *whole heap*.
//!
//! That asymmetry is the defining property of this collector. A program with a
//! small live set in a large heap marks almost nothing and sweeps everything.
//!
//! ## Why the worklist is explicit
//!
//! The obvious way to write the mark phase is a recursive function that calls
//! itself for each child. Do not: the recursion depth is the depth of the
//! object graph, which is under the mutator's control, and a linked list of a
//! million nodes will exhaust the native stack. Collectors cannot fail while
//! collecting — they are usually running *because* memory is short — so the
//! traversal carries its own worklist on the heap.
//!
//! ## What the mark bit costs
//!
//! The bit lives in the object header and it belongs to one collection. The
//! next collection starts from a completely different live set, so every bit
//! set by this collection has to be back to zero before the next one begins.
//! There are two moments to do that, and choosing neither is the classic way to
//! build a collector that works perfectly once.
//!
//! ## Sweeping and the heap walk
//!
//! The sweep depends on the invariant from module 1: every block in
//! `[0, bump)`, live or free, carries its true size, so the heap can be walked
//! by repeatedly reading a size and stepping that far. Freed blocks are merged
//! afterwards, or the heap would fill with holes too small to reuse.
//!
//! ## Your work
//!
//! [`MarkSweep::mark_from_roots`] is unimplemented. The rest of the module is
//! written but does not pass its tests.

use std::time::Instant;

use gc_core::collector::{Collector, GcError};
use gc_core::heap::{Handle, Heap};
use gc_core::roots::Roots;
use gc_core::space::FreeListSpace;
use gc_core::stats::GcStats;

/// Never collect while the heap holds less than this, so that tiny programs do
/// not pay for a collection they do not need.
const MIN_THRESHOLD: u32 = 64 << 10;

/// How many collections in a row may reclaim almost nothing before the
/// collector gives up and reports the heap full.
///
/// Without a limit like this, a program whose live set genuinely fills the heap
/// makes the collector run before every single allocation, each one walking the
/// whole heap to reclaim a few bytes. The program does not stop, it just stops
/// getting anywhere — which looks like a hang rather than an error. The JVM
/// reports the same situation as "GC overhead limit exceeded".
const THRASH_LIMIT: u32 = 4;

pub struct MarkSweep {
    space: FreeListSpace,
    roots: Roots,
    stats: GcStats,
    /// Objects reached but not yet scanned. A field rather than a local so the
    /// allocation is reused across collections.
    worklist: Vec<Handle>,
    /// Collect once this many bytes are in use.
    threshold: u32,
    /// Consecutive collections that reclaimed almost nothing from a nearly
    /// full heap. See [`THRASH_LIMIT`].
    thrashing: u32,
}

impl MarkSweep {
    pub fn new(capacity: usize) -> MarkSweep {
        MarkSweep {
            space: FreeListSpace::new(capacity),
            roots: Roots::new(),
            stats: GcStats::default(),
            worklist: Vec::new(),
            threshold: MIN_THRESHOLD.min(capacity as u32 / 2),
            thrashing: 0,
        }
    }

    pub fn space(&self) -> &FreeListSpace {
        &self.space
    }
    pub fn space_mut(&mut self) -> &mut FreeListSpace {
        &mut self.space
    }
    pub fn is_freed(&self, h: Handle) -> bool {
        self.space.is_free(h)
    }
    pub fn is_marked(&self, h: Handle) -> bool {
        self.space.heap().is_marked(h)
    }
    /// Bytes in use that will trigger the next collection.
    pub fn threshold(&self) -> u32 {
        self.threshold
    }

    /// Record a successful allocation and hand the object back.
    fn account(&mut self, h: Handle) -> Handle {
        self.stats.allocations += 1;
        self.stats.bytes_allocated += self.space.heap().size(h) as u64;
        self.stats.observe_used(self.space.used_bytes());
        h
    }

    /// Every handle the mutator can reach without going through another object.
    ///
    /// Tracing starts here, so anything missing from this list is invisible to
    /// the collector however reachable the program considers it.
    fn root_set(&self) -> Vec<Handle> {
        self.roots.stack_snapshot().iter().copied().filter(|h| !h.is_null()).collect()
    }

    /// Set the mark bit on every object reachable from the roots.
    ///
    /// The traversal must not recurse: the object graph can be as deep as the
    /// heap is large. An object is only worth scanning once, however many
    /// references point at it.
    /// Count every object visited in [`gc_core::stats::GcStats::objects_marked`].
    pub fn mark_from_roots(&mut self) {
        let roots = self.root_set();
        self.worklist.clear();
        self.worklist.extend(roots);

        todo!("scan the worklist until it is empty, marking everything reachable")
    }

    /// Walk the heap and return every unmarked object to the allocator.
    fn sweep(&mut self) {
        let mut doomed: Vec<Handle> = Vec::new();

        // A linear walk from the bottom of the heap. Every block, live or
        // free, carries its own size, and that is what makes the next block
        // findable without any index of where objects are.
        let bump = self.space.bump_pointer();
        let mut at = 0u32;
        while at < bump {
            let h = Handle(at);
            at += self.space.heap().size(h);

            if self.space.is_free(h) {
                continue;
            }
            if !self.space.heap().is_marked(h) {
                doomed.push(h);
            }
        }

        for h in doomed {
            let size = self.space.heap().size(h);
            self.space.free(h);
            self.stats.objects_freed += 1;
            self.stats.bytes_freed += size as u64;
        }

        self.space.coalesce();
    }
}

impl Collector for MarkSweep {
    fn name(&self) -> &'static str {
        "mark and sweep"
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
        let worth_collecting = self.thrashing < THRASH_LIMIT;

        if worth_collecting && self.space.used_bytes() >= self.threshold {
            self.collect();
        }
        if let Some(h) = self.space.alloc(nrefs, ndata) {
            return Ok(self.account(h));
        }
        if worth_collecting {
            self.collect();
            if let Some(h) = self.space.alloc(nrefs, ndata) {
                return Ok(self.account(h));
            }
        }

        self.stats.allocation_failures += 1;
        Err(GcError::OutOfMemory {
            requested: Heap::size_for(nrefs, ndata),
            used: self.space.used_bytes(),
            capacity: self.space.capacity(),
        })
    }

    /// Tracing collectors need no write barrier: the next collection walks the
    /// graph from scratch and sees whatever the mutator has built.
    fn write_field(&mut self, obj: Handle, i: u16, val: Handle) {
        self.heap_mut().set_field(obj, i, val);
    }

    fn collect(&mut self) {
        let start = Instant::now();
        let before = self.space.used_bytes();

        self.mark_from_roots();
        self.sweep();

        // Aim to let the heap roughly double before collecting again, so that
        // collection cost stays proportional to allocation rather than to how
        // often the program happens to call this.
        let live = self.space.used_bytes();
        self.threshold = live.saturating_mul(2).max(MIN_THRESHOLD).min(self.space.capacity());

        let capacity = self.space.capacity();
        let reclaimed = before.saturating_sub(live);
        if live > capacity / 16 * 15 && reclaimed < capacity / 32 {
            self.thrashing += 1;
        } else {
            self.thrashing = 0;
        }

        self.stats.collections += 1;
        self.stats.major_collections += 1;
        self.stats.record_pause(start.elapsed());
    }
}
