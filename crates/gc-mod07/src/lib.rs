//! # Module 7 — Generational collection
//!
//! Almost all objects die young. That single observation, called the **weak
//! generational hypothesis**, is why production collectors are fast, and this
//! module is where the course stops improving the collector and starts
//! exploiting the program.
//!
//! Every collector so far treated the heap as uniform: module 4 swept all of
//! it, module 6 copied everything that survived. But survival is not uniformly
//! distributed. A freshly allocated object is overwhelmingly likely to be
//! garbage within moments; an object that has already survived several
//! collections is likely to survive the next one too. Spending equal effort on
//! both is the waste this module removes.
//!
//! So the heap is divided by *age*. New objects go in a small **nursery**.
//! When it fills, a **minor collection** looks only at the nursery: whatever is
//! still reachable is promoted into the **old generation**, and the entire
//! nursery is then reclaimed in one stroke, without examining a single dead
//! object. Because the nursery is small and almost everything in it is dead,
//! this is fast and it is frequent. The old generation is collected rarely, by
//! a **major collection**, which is the mark-sweep of module 4.
//!
//! ## The problem that tracing cannot solve
//!
//! A minor collection traces from the roots, but it deliberately does not
//! trace the old generation — looking at every old object is exactly the cost
//! it exists to avoid. That creates a hole.
//!
//! ```text
//!     roots ────► old object ────► young object
//!                (not traced)      (never discovered)
//! ```
//!
//! An old object holding the only reference to a young one is invisible to a
//! collection that starts at the roots and skips the old generation. The young
//! object is reclaimed while something still points at it.
//!
//! No amount of cleverness in the collector can find these edges, because the
//! collector never looks. The information has to come from the mutator, at the
//! moment the reference is created.
//!
//! ## The write barrier
//!
//! Every store into a reference slot runs a fragment of collector code first.
//! If it stores a young address into an old object, that old object is added
//! to the **remembered set** — a list of old objects that might point into the
//! nursery. A minor collection treats the remembered set as extra roots.
//!
//! This is the first place in the course where the collector imposes a cost on
//! ordinary program code, and it is a cost paid on *every* reference store,
//! forever, to make collections cheaper. That trade is why it is worth being
//! precise: a barrier that remembers old-to-old or young-to-young edges is not
//! wrong, but it fills the remembered set with entries that can never matter,
//! and a minor collection then scans them all.
//!
//! An object remembered twice must not appear in the set twice, or a loop that
//! repeatedly overwrites the same field grows it without bound.
//!
//! ## Promotion moves objects
//!
//! Surviving the nursery means being copied into the old generation, so this
//! module relocates, and everything module 6 said about that applies: an
//! object reachable by two paths must be promoted once, with every reference
//! updated to the same new address. The forwarding pointer does that job here
//! too.
//!
//! ## Your work
//!
//! [`Generational::evacuate_nursery`] is unimplemented. The rest of the module
//! is written but does not pass its tests.

use std::time::Instant;

use gc_core::collector::{Collector, GcError};
use gc_core::heap::{FLAG_BUFFERED, FLAG_FREE, Handle, Heap};
use gc_core::roots::Roots;
use gc_core::stats::GcStats;

/// The nursery is this fraction of the heap. It is deliberately small, so that
/// minor collections happen often enough to watch.
const NURSERY_FRACTION: u32 = 64;

/// Collect the old generation once it is this full, as a percentage.
const MAJOR_THRESHOLD_PERCENT: u32 = 75;

/// Generation number of an object that has never survived a collection.
pub const YOUNG: u8 = 0;
/// Generation number of a promoted object.
pub const OLD: u8 = 1;

pub struct Generational {
    heap: Heap,
    roots: Roots,
    stats: GcStats,

    /// First address of the nursery. Everything below it is the old
    /// generation; everything from it up is young.
    nursery: u32,
    /// Next free address in the nursery.
    young_free: u32,
    /// Objects currently allocated in the nursery.
    young_objects: u64,

    /// Free blocks in the old generation.
    free: Vec<Handle>,
    /// First old-generation address never yet handed out.
    old_bump: u32,
    /// Bytes committed to live objects in the old generation.
    old_used: u32,

    /// Old objects that may hold a reference into the nursery.
    remembered: Vec<Handle>,
    /// Promoted objects whose fields have yet to be examined.
    worklist: Vec<Handle>,
}

impl Generational {
    pub fn new(capacity: usize) -> Generational {
        let heap = Heap::new(capacity);
        let cap = heap.capacity();
        let nursery_bytes = gc_core::heap::align_up(cap / NURSERY_FRACTION);
        assert!(
            nursery_bytes > 0 && nursery_bytes < cap,
            "heap too small to divide"
        );
        let nursery = cap - nursery_bytes;
        Generational {
            heap,
            roots: Roots::new(),
            stats: GcStats::default(),
            nursery,
            young_free: nursery,
            young_objects: 0,
            free: Vec::new(),
            old_bump: 0,
            old_used: 0,
            remembered: Vec::new(),
            worklist: Vec::new(),
        }
    }

    /// First address of the nursery.
    pub fn nursery_start(&self) -> u32 {
        self.nursery
    }
    /// Total bytes in the nursery.
    pub fn nursery_size(&self) -> u32 {
        self.heap.capacity() - self.nursery
    }
    /// Bytes of the nursery handed out since the last collection.
    pub fn young_used(&self) -> u32 {
        self.young_free - self.nursery
    }
    /// Bytes committed to live objects in the old generation.
    pub fn old_used(&self) -> u32 {
        self.old_used
    }
    /// True if `h` lives in the nursery.
    pub fn is_young(&self, h: Handle) -> bool {
        h.addr() >= self.nursery
    }
    /// Old objects currently recorded as pointing into the nursery.
    pub fn remembered_count(&self) -> usize {
        self.remembered.len()
    }
    /// True if `h` is recorded in the remembered set.
    pub fn is_remembered(&self, h: Handle) -> bool {
        self.heap.flag(h, FLAG_BUFFERED)
    }
    /// Every live object in the old generation, in address order.
    pub fn old_objects(&self) -> Vec<Handle> {
        self.heap
            .walk(0, self.old_bump)
            .filter(|&h| !self.heap.flag(h, FLAG_FREE))
            .collect()
    }

    /// Reserve `size` bytes in the old generation.
    fn old_alloc(&mut self, size: u32) -> Option<u32> {
        if let Some(idx) = self.free.iter().position(|&b| self.heap.size(b) >= size) {
            let block = self.free.swap_remove(idx);
            let remainder = self.heap.size(block) - size;
            if remainder > 0 {
                let split = self.heap.emplace_block(block.0 + size, remainder);
                self.free.push(split);
            }
            self.old_used += size;
            return Some(block.0);
        }
        if self.old_bump + size <= self.nursery {
            let at = self.old_bump;
            self.old_bump += size;
            self.old_used += size;
            return Some(at);
        }
        None
    }

    /// Move a young object into the old generation, or report where it already
    /// went. Reached once for every reference to the object, so it must give
    /// the same answer every time.
    pub fn promote(&mut self, h: Handle) -> Handle {
        let already = self.heap.forward(h);
        if !already.is_null() {
            return already;
        }

        let size = self.heap.size(h);
        let at = self.old_alloc(size).unwrap_or_else(|| {
            panic!(
                "the old generation is full: promoting {h:?} needs {size} bytes and none of \
                 the {} bytes below the nursery are free",
                self.nursery
            )
        });

        let copy = self.heap.relocate(h, at);
        self.heap.set_forward(h, copy);
        self.heap.set_generation(copy, OLD);
        self.stats.objects_promoted += 1;
        self.stats.objects_copied += 1;
        self.worklist.push(copy);
        copy
    }

    /// Promote everything in the nursery that is still reachable, and leave the
    /// nursery empty.
    ///
    /// A minor collection starts from the shadow stack, the global table *and*
    /// the remembered set, because an old object's reference into the nursery
    /// is reachable by the program but not by a traversal that skips the old
    /// generation. Promoting an object moves it, so every reference that named
    /// it has to be updated to its new address — including the reference in the
    /// root slot or old object the traversal arrived through.
    ///
    /// A promoted object may itself refer to other young objects, so its own
    /// fields must be examined after it moves. [`Generational::worklist`] holds
    /// the promoted objects that have yet to be looked at.
    ///
    /// Whatever was not promoted died in the nursery and is reclaimed by
    /// resetting a pointer, without being examined at all. Those objects still
    /// have to be counted in [`gc_core::stats::GcStats::objects_freed`] and
    /// `bytes_freed`, and the nursery must be left empty.
    pub fn evacuate_nursery(&mut self) {
        todo!("promote everything in the nursery that is still reachable")
    }

    /// Replace every young reference held by `obj` with the promoted address.
    pub fn scan_for_young(&mut self, obj: Handle) {
        for i in 0..self.heap.nrefs(obj) {
            let child = self.heap.field(obj, i);
            if !child.is_null() && self.is_young(child) {
                let moved = self.promote(child);
                self.heap.set_field(obj, i, moved);
            }
        }
    }

    /// Collect the nursery only.
    pub fn minor_collect(&mut self) {
        let start = Instant::now();
        self.evacuate_nursery();
        self.stats.collections += 1;
        self.stats.minor_collections += 1;
        self.stats.record_pause(start.elapsed());
    }

    /// Collect the nursery and then the old generation.
    pub fn major_collect(&mut self) {
        let start = Instant::now();
        self.evacuate_nursery();
        self.mark_old();
        self.sweep_old();
        self.stats.collections += 1;
        self.stats.major_collections += 1;
        self.stats.record_pause(start.elapsed());
    }

    /// Mark everything reachable. After evacuation every live object is old.
    fn mark_old(&mut self) {
        self.worklist.clear();
        self.worklist.extend(self.roots.iter());
        while let Some(h) = self.worklist.pop() {
            if self.heap.is_marked(h) {
                continue;
            }
            self.heap.set_marked(h, true);
            self.stats.objects_marked += 1;
            for c in self.heap.children(h) {
                if !self.heap.is_marked(c) {
                    self.worklist.push(c);
                }
            }
        }
    }

    /// Return every unmarked old object to the free list, merging neighbours.
    fn sweep_old(&mut self) {
        let end = self.old_bump;
        let mut free = Vec::new();
        let mut run: Option<u32> = None;
        let mut used = 0u32;
        let mut at = 0u32;
        while at < end {
            let h = Handle(at);
            let size = self.heap.size(h);
            let was_free = self.heap.flag(h, FLAG_FREE);
            if !was_free && self.heap.is_marked(h) {
                self.heap.set_marked(h, false);
                used += size;
                if let Some(s) = run.take() {
                    free.push(self.heap.emplace_block(s, at - s));
                }
            } else {
                if !was_free {
                    self.stats.objects_freed += 1;
                    self.stats.bytes_freed += size as u64;
                    self.heap.poison(h);
                }
                run.get_or_insert(at);
            }
            at += size;
        }
        if let Some(s) = run {
            free.push(self.heap.emplace_block(s, end - s));
        }
        self.free = free;
        self.old_used = used;
    }

    fn account(&mut self, h: Handle) -> Handle {
        self.stats.allocations += 1;
        self.stats.bytes_allocated += self.heap.size(h) as u64;
        self.stats.observe_used(self.used_bytes());
        h
    }
}

impl Collector for Generational {
    fn name(&self) -> &'static str {
        "generational"
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

    fn used_bytes(&self) -> u32 {
        self.old_used + self.young_used()
    }

    fn alloc(&mut self, nrefs: u16, ndata: u32) -> Result<Handle, GcError> {
        let need = Heap::size_for(nrefs, ndata);

        // Anything too large to ever fit in the nursery is born old.
        if need > self.nursery_size() {
            if self.old_alloc(need).is_none() {
                self.major_collect();
            }
            return match self.old_alloc(need) {
                Some(at) => {
                    let h = self.heap.emplace(at, nrefs, ndata);
                    self.heap.set_generation(h, OLD);
                    Ok(self.account(h))
                }
                None => {
                    self.stats.allocation_failures += 1;
                    Err(GcError::OutOfMemory {
                        requested: need,
                        used: self.used_bytes(),
                        capacity: self.heap.capacity(),
                    })
                }
            };
        }

        if self.young_free + need > self.heap.capacity() {
            self.minor_collect();
            if self.old_used * 100 > self.nursery * MAJOR_THRESHOLD_PERCENT {
                self.major_collect();
            }
        }
        if self.young_free + need > self.heap.capacity() {
            self.stats.allocation_failures += 1;
            return Err(GcError::OutOfMemory {
                requested: need,
                used: self.used_bytes(),
                capacity: self.heap.capacity(),
            });
        }

        let at = self.young_free;
        self.young_free += need;
        self.young_objects += 1;
        let h = self.heap.emplace(at, nrefs, ndata);
        Ok(self.account(h))
    }

    /// The write barrier.
    ///
    /// An old object that comes to hold a young reference is the one edge a
    /// minor collection cannot discover for itself, so it is recorded here.
    fn write_field(&mut self, obj: Handle, i: u16, val: Handle) {
        self.stats.barrier_hits += 1;
        // A store that puts a nursery address somewhere is the one a minor
        // collection cannot find for itself, so the holder is recorded.
        if !val.is_null() && self.is_young(val) {
            self.heap.set_flag(obj, FLAG_BUFFERED, true);
            self.remembered.push(obj);
            self.stats.remembered_entries = self.remembered.len() as u64;
        }
        self.heap.set_field(obj, i, val);
    }

    fn collect(&mut self) {
        self.major_collect();
    }
}
