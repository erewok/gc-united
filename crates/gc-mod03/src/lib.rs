//! # Module 3 — Cycle collection by trial deletion
//!
//! Module 2 ended with a hole: a group of objects that reference each other
//! keeps its own counts above zero forever, so reference counting alone leaks
//! every cycle. This module closes it, using the algorithm CPython uses in
//! spirit and Bacon and Rajan formalised in *Concurrent Cycle Collection in
//! Reference Counted Systems* (2001).
//!
//! ## The idea
//!
//! A reference count tells you how many references point at an object, but not
//! where they come from. For a garbage cycle, every one of those references
//! comes from inside the cycle itself.
//!
//! So: take a suspect subgraph and *trially delete* the references that live
//! inside it. Walk the subgraph, and for every edge you cross, decrement the
//! target's count. What remains in each count is the number of references from
//! outside the subgraph. Any object left at zero is reachable only from within
//! — it is garbage. Any object left above zero is genuinely referenced from
//! elsewhere, so it and everything it reaches must be restored by adding all
//! those references back.
//!
//! That is the whole algorithm. The bookkeeping exists to do it without
//! visiting an object twice and without ever leaving a count wrong.
//!
//! ## Colours
//!
//! Each object carries a colour, and these are *not* the tri-colour marks of
//! module 8:
//!
//! - [`BLACK`] — in use, or already dealt with.
//! - [`PURPLE`] — a possible root of a cycle: something released a reference to
//!   it and it did not die. Every candidate starts here.
//! - [`GRAY`] — currently being trially deleted.
//! - [`WHITE`] — proven to be a member of a garbage cycle.
//!
//! ## Which objects are suspects
//!
//! Only an object whose count is *decremented without reaching zero* can be the
//! root of a garbage cycle: if it reached zero it was already freed, and if
//! nothing was decremented nothing changed. So [`CycleCollector::release`]
//! files exactly those objects in the candidate buffer, and a collection
//! considers only what is in that buffer. This is why cycle collection is
//! cheap: it looks at suspects, not at the whole heap.
//!
//! ## The three passes
//!
//! Each pass runs over every candidate before the next begins. Doing them
//! object-by-object instead would restore counts that a later candidate still
//! needs to see deleted.
//!
//! 1. **Mark** — [`CycleCollector::mark_gray`] on each candidate: colour the
//!    subgraph gray, decrementing as each edge is crossed.
//! 2. **Scan** — [`CycleCollector::scan`] on each candidate: anything still
//!    above zero is externally referenced, so [`CycleCollector::scan_black`]
//!    puts it and everything below it back; anything at zero goes white.
//! 3. **Collect** — [`CycleCollector::collect_white`] on each candidate: free
//!    what is still white.
//!
//! ## Recursion
//!
//! All four traversals are written with explicit worklists. A cycle can be as
//! long as the heap, and a collector that recurses over one dies on a
//! structure the mutator built perfectly happily.
//!
//! ## Your work
//!
//! [`CycleCollector::mark_gray`] and [`CycleCollector::collect_white`] are
//! unimplemented. The rest of the module is written but does not pass its
//! tests.

use std::time::Instant;

use gc_core::collector::{Collector, GcError};
use gc_core::heap::{FLAG_BUFFERED, Handle, Heap};
use gc_core::roots::Roots;
use gc_core::space::FreeListSpace;
use gc_core::stats::GcStats;

/// In use, or already dealt with by this collection.
pub const BLACK: u8 = 0;
/// Being trially deleted.
pub const GRAY: u8 = 1;
/// Proven to be a member of a garbage cycle.
pub const WHITE: u8 = 2;
/// A possible root of a cycle, waiting in the candidate buffer.
pub const PURPLE: u8 = 3;

pub struct CycleCollector {
    space: FreeListSpace,
    roots: Roots,
    stats: GcStats,
    pending: Vec<Handle>,
    /// Objects that lost a reference without dying. Only these are examined.
    candidates: Vec<Handle>,
    cycles_collected: u64,
}

impl CycleCollector {
    pub fn new(capacity: usize) -> CycleCollector {
        CycleCollector {
            space: FreeListSpace::new(capacity),
            roots: Roots::new(),
            stats: GcStats::default(),
            pending: Vec::new(),
            candidates: Vec::new(),
            cycles_collected: 0,
        }
    }

    pub fn rc_of(&self, h: Handle) -> u32 {
        self.space.heap().rc(h)
    }
    pub fn is_freed(&self, h: Handle) -> bool {
        self.space.is_free(h)
    }
    pub fn color_of(&self, h: Handle) -> u8 {
        self.space.heap().color(h)
    }
    /// Objects currently waiting to be examined as possible cycle roots.
    pub fn candidate_count(&self) -> usize {
        self.candidates.len()
    }
    /// Objects freed because they belonged to a garbage cycle.
    pub fn cycles_collected(&self) -> u64 {
        self.cycles_collected
    }
    pub fn space(&self) -> &FreeListSpace {
        &self.space
    }

    fn color(&self, h: Handle) -> u8 {
        self.space.heap().color(h)
    }
    fn set_color(&mut self, h: Handle, c: u8) {
        self.space.heap_mut().set_color(h, c);
    }
    fn buffered(&self, h: Handle) -> bool {
        self.space.heap().flag(h, FLAG_BUFFERED)
    }
    fn set_buffered(&mut self, h: Handle, on: bool) {
        self.space.heap_mut().set_flag(h, FLAG_BUFFERED, on);
    }
    fn children(&self, h: Handle) -> Vec<Handle> {
        self.space.heap().children(h)
    }

    /// Add one to `h`'s count. An object that has just gained a reference is
    /// in use, so it is no longer a suspect.
    pub fn retain(&mut self, h: Handle) {
        if h.is_null() {
            return;
        }
        let rc = self.space.heap().rc(h);
        self.space.heap_mut().set_rc(h, rc + 1);
        self.set_color(h, BLACK);
    }

    /// Take one off `h`'s count, freeing it if that was the last reference and
    /// filing it as a possible cycle root if it was not.
    pub fn release(&mut self, h: Handle) {
        if h.is_null() {
            return;
        }
        self.pending.push(h);

        while let Some(obj) = self.pending.pop() {
            let rc = self.space.heap().rc(obj);
            assert!(
                rc > 0,
                "{obj:?} was released while its count was already zero"
            );
            self.space.heap_mut().set_rc(obj, rc - 1);

            if rc == 1 {
                let children = self.children(obj);
                self.pending.extend(children);
                self.free_object(obj);
            } else {
                self.possible_root(obj);
            }
        }
    }

    /// File `s` as a possible root of a garbage cycle.
    fn possible_root(&mut self, s: Handle) {
        if self.color(s) != PURPLE {
            self.set_color(s, PURPLE);
            if !self.buffered(s) {
                self.set_buffered(s, true);
                self.candidates.push(s);
            }
        }
    }

    fn free_object(&mut self, h: Handle) {
        let size = self.space.heap().size(h);
        // A freed block may still be listed as a candidate. Clearing the flag
        // is what tells the next collection that the entry is stale.
        self.set_buffered(h, false);
        self.space.free(h);
        self.stats.objects_freed += 1;
        self.stats.bytes_freed += size as u64;
    }

    // ---- the three passes -------------------------------------------------

    /// Colour `root`'s subgraph gray, taking one off each object's count for
    /// every reference crossed on the way in.
    ///
    /// After this, each count in the subgraph has had all *internal* references
    /// subtracted from it. An object already gray has had its own children
    /// dealt with, but an edge arriving at it still has to be subtracted.
    pub fn mark_gray(&mut self, root: Handle) {
        let mut work = vec![root];
        while let Some(s) = work.pop() {
            if self.color(s) != GRAY {
                self.set_color(s, GRAY);
                for t in self.children(s) {
                    let rc = self.space.heap().rc(s);
                    self.space.heap_mut().set_rc(s, rc - 1);
                    work.push(t);
                }
            }
        }
    }

    /// Decide, for each object in `root`'s gray subgraph, whether it survives.
    ///
    /// A count still above zero means a reference from outside the subgraph
    /// survived trial deletion, so the object lives and everything it reaches
    /// has to be put back. A count at zero means every reference to it came
    /// from inside: it goes white.
    pub fn scan(&mut self, root: Handle) {
        let mut work = vec![root];
        while let Some(s) = work.pop() {
            if self.color(s) != GRAY {
                continue;
            }
            if self.space.heap().rc(s) > 0 {
                self.scan_black(s);
            } else {
                self.set_color(s, WHITE);
                work.extend(self.children(s));
            }
        }
    }

    /// Undo trial deletion across `root`'s subgraph: it is externally
    /// referenced after all, so every reference it holds must be restored.
    pub fn scan_black(&mut self, root: Handle) {
        self.set_color(root, BLACK);
        let mut work = vec![root];
        while let Some(s) = work.pop() {
            for t in self.children(s) {
                if self.color(t) != BLACK {
                    let rc = self.space.heap().rc(t);
                    self.space.heap_mut().set_rc(t, rc + 1);
                    self.set_color(t, BLACK);
                    work.push(t);
                }
            }
        }
    }

    /// Free everything still white in `root`'s subgraph.
    ///
    /// An object that is still buffered is some other candidate's problem and
    /// must be left alone; it will be reached when that candidate's turn comes.
    /// Nothing may be freed until the traversal is finished, because freeing an
    /// object destroys the references the traversal is walking.
    /// Count every object freed here in [`CycleCollector::cycles_collected`].
    pub fn collect_white(&mut self, root: Handle) {
        let mut work = vec![root];
        while let Some(s) = work.pop() {
            if self.color(s) == WHITE && !self.candidates.contains(&s) {
                self.set_color(s, BLACK);
                for t in self.children(s) {
                    work.push(t);
                }
                self.space.free(s);
            }
        }
    }

    /// Examine every candidate and free whatever turns out to be a garbage
    /// cycle.
    pub fn collect_cycles(&mut self) {
        let candidates = std::mem::take(&mut self.candidates);

        // Pass 1: trial deletion over every suspect subgraph.
        let mut cycle_roots: Vec<Handle> = Vec::new();
        for &s in &candidates {
            // An entry is stale if the block was freed, or reused for another
            // object, since it was filed.
            if !self.buffered(s) || self.space.is_free(s) {
                continue;
            }
            if self.color(s) == PURPLE && self.space.heap().rc(s) > 0 {
                self.mark_gray(s);
                cycle_roots.push(s);
            }
        }

        // Pass 2: work out which of them were really unreachable.
        for &s in &cycle_roots {
            self.scan(s);
        }

        // Pass 3: free them.
        for &s in &cycle_roots {
            self.set_buffered(s, false);
            self.collect_white(s);
        }
    }
}

impl Collector for CycleCollector {
    fn name(&self) -> &'static str {
        "reference counting with cycle collection"
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
        if let Some(h) = self.space.alloc(nrefs, ndata) {
            self.stats.allocations += 1;
            self.stats.bytes_allocated += self.space.heap().size(h) as u64;
            self.stats.observe_used(self.space.used_bytes());
            return Ok(h);
        }
        self.collect();
        match self.space.alloc(nrefs, ndata) {
            Some(h) => {
                self.stats.allocations += 1;
                self.stats.bytes_allocated += self.space.heap().size(h) as u64;
                self.stats.observe_used(self.space.used_bytes());
                Ok(h)
            }
            None => {
                self.stats.allocation_failures += 1;
                Err(GcError::OutOfMemory {
                    requested: Heap::size_for(nrefs, ndata),
                    used: self.space.used_bytes(),
                    capacity: self.space.capacity(),
                })
            }
        }
    }

    fn write_field(&mut self, obj: Handle, i: u16, val: Handle) {
        self.retain(val);
        let old = self.heap().field(obj, i);
        self.heap_mut().set_field(obj, i, val);
        self.release(old);
    }

    fn on_global_changed(&mut self, old: Handle, new: Handle) {
        self.retain(new);
        self.release(old);
    }

    fn on_root_pushed(&mut self, h: Handle) {
        self.retain(h);
    }

    fn on_root_popped(&mut self, h: Handle) {
        self.release(h);
    }

    fn collect(&mut self) {
        let start = Instant::now();
        self.collect_cycles();
        self.space.coalesce();
        self.stats.collections += 1;
        self.stats.major_collections += 1;
        self.stats.record_pause(start.elapsed());
    }
}
