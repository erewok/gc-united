//! # Module 6 — Semispace copying
//!
//! Divide the heap in two and only ever use one half. Allocate by bumping a
//! pointer through that half; when it fills, copy everything still live into
//! the other half, and swap. The half you just left is garbage in its entirety
//! and needs no examination at all.
//!
//! This is the cheapest collector in the course, and the reason is worth
//! stating precisely: **its cost is proportional to what survives, not to what
//! was allocated.** Module 4 swept the whole heap; module 5 walked it three
//! times. A copying collector never looks at a dead object. A program that
//! allocates a hundred thousand objects and keeps ten does ten objects' worth
//! of work — the other ninety thousand nine hundred and ninety cost nothing
//! whatsoever to reclaim.
//!
//! Compaction comes free, because copying into a fresh space packs objects
//! against each other by construction. Allocation stays a bump pointer.
//!
//! The price is stark: half the memory is unused at all times, and every
//! survivor is copied on every collection. That is exactly the wrong trade for
//! long-lived data and exactly the right one for short-lived data — which is
//! why module 7 uses this for young objects and something else for old ones.
//!
//! ## Cheney's algorithm
//!
//! The obvious implementation copies recursively and needs a stack as deep as
//! the object graph. Cheney's does not need one at all, and the trick is
//! beautiful: **to-space is the worklist.**
//!
//! Keep two cursors into to-space. `free` is where the next copy will go.
//! `scan` is how far the fields have been updated. Everything between them is
//! copied but not yet examined — precisely the grey set of module 8, stored for
//! nothing in space you were going to use anyway.
//!
//! ```text
//!   to-space:  [ scanned | copied, not yet scanned |  unused  ]
//!              ^                                   ^
//!              scan                                free
//! ```
//!
//! Copy the roots, which advances `free`. Then repeatedly take the object at
//! `scan`, replace each of its references with a copy of the target, and
//! advance `scan` past it. Copying a target advances `free`. When `scan`
//! catches up with `free`, everything reachable has been copied and updated.
//!
//! Note the consequence: `free` moves *during* the loop. A loop that reads it
//! once at the start stops early, and stops early exactly at the objects
//! copied last.
//!
//! ## Forwarding pointers
//!
//! An object reachable by two paths must be copied **once**, and both
//! references must end up naming the same copy. Otherwise the program's object
//! graph silently changes shape: one object becomes two, a write through one
//! reference is invisible through the other, and nothing crashes.
//!
//! The mechanism is a forwarding pointer. When an object is copied, its
//! *original* is overwritten with the address of the copy. That original is
//! never used for anything else — its space is about to be abandoned — so it
//! serves as a note reading "already moved, it is over there". Every subsequent
//! reference to that object finds the note instead of copying again.
//!
//! Which of the two objects carries the note matters. Every reference that has
//! yet to be followed still points into from-space, so from-space is the only
//! place a note will ever be looked for.
//!
//! ## Your work
//!
//! [`SemiSpace::scan_to_space`] is unimplemented. The rest of the module is
//! written but does not pass its tests.

use std::time::Instant;

use gc_core::collector::{Collector, GcError};
use gc_core::heap::{Handle, Heap};
use gc_core::roots::Roots;
use gc_core::stats::GcStats;

pub struct SemiSpace {
    heap: Heap,
    roots: Roots,
    stats: GcStats,
    /// Size of each half.
    half: u32,
    /// Start of the half currently being allocated into.
    current: u32,
    /// Start of the half currently standing idle.
    idle: u32,
    /// Next free address in the current half.
    free: u32,
    /// During a collection, how far to-space has been scanned.
    scan: u32,
    /// Objects currently in the space being allocated into.
    live_objects: u64,
    collections_with_no_survivors: u64,
}

impl SemiSpace {
    pub fn new(capacity: usize) -> SemiSpace {
        let heap = Heap::new(capacity);
        let half = gc_core::heap::align_up(heap.capacity() / 2);
        assert!(half * 2 <= heap.capacity(), "each half must fit");
        SemiSpace {
            heap,
            roots: Roots::new(),
            stats: GcStats::default(),
            half,
            current: 0,
            idle: half,
            free: 0,
            scan: 0,
            live_objects: 0,
            collections_with_no_survivors: 0,
        }
    }

    /// Bytes in each half. Only one half is usable at a time.
    pub fn half_size(&self) -> u32 {
        self.half
    }
    /// Start of the half currently being allocated into.
    pub fn current_space(&self) -> u32 {
        self.current
    }
    /// Start of the half standing idle.
    pub fn idle_space(&self) -> u32 {
        self.idle
    }
    /// Next free address in the current half.
    pub fn free_pointer(&self) -> u32 {
        self.free
    }
    /// One past the last usable byte of the current half.
    pub fn limit(&self) -> u32 {
        self.current + self.half
    }
    /// True if `h` lies in the half currently being allocated into.
    pub fn in_current_space(&self, h: Handle) -> bool {
        h.addr() >= self.current && h.addr() < self.current + self.half
    }
    /// Every live object, in address order.
    pub fn objects(&self) -> Vec<Handle> {
        self.heap.walk(self.current, self.free).collect()
    }
    /// Collections after which nothing at all survived.
    pub fn empty_collections(&self) -> u64 {
        self.collections_with_no_survivors
    }

    /// Ensure `h` has a copy in to-space and return where that copy is.
    ///
    /// Called for every reference encountered, so it is reached many times for
    /// an object several things point at, and must produce the same answer
    /// every time.
    fn copy_object(&mut self, h: Handle) -> Handle {
        // An object that already sits in the half being copied into has been
        // dealt with, and its own address is the answer.
        if self.in_current_space(h) {
            return h;
        }

        let size = self.heap.size(h);
        let dest = self.free;
        assert!(
            dest + size <= self.limit(),
            "to-space overflowed while copying {h:?}: more data survived than fits in one \
             half ({} bytes) of a {} byte heap",
            self.half,
            self.heap.capacity()
        );

        let copy = self.heap.relocate(h, dest);
        self.free += size;
        self.stats.objects_copied += 1;

        // Leave a note in the original saying where the object went. Every
        // reference still waiting to be followed points at the original.
        self.heap.set_forward(h, Handle(dest));
        copy
    }

    /// Run the Cheney loop until to-space holds a complete, self-consistent
    /// copy of everything reachable.
    ///
    /// On entry, the roots have been copied, so `scan` is at the start of
    /// to-space and `free` is past the last root copied. Take the object at
    /// `scan`, replace each of its references with the address of that target's
    /// copy, and move `scan` past it. Copying a target moves `free` further
    /// away. The graph is complete when `scan` reaches `free`.
    fn scan_to_space(&mut self) {
        todo!("copy and update everything reachable, until scan catches up with free")
    }

    fn account(&mut self, h: Handle) -> Handle {
        self.stats.allocations += 1;
        self.live_objects += 1;
        self.stats.bytes_allocated += self.heap.size(h) as u64;
        self.stats.observe_used(self.free - self.current);
        h
    }
}

impl Collector for SemiSpace {
    fn name(&self) -> &'static str {
        "semispace copying"
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
        self.free
    }

    fn alloc(&mut self, nrefs: u16, ndata: u32) -> Result<Handle, GcError> {
        let need = Heap::size_for(nrefs, ndata);
        if self.free + need > self.limit() {
            self.collect();
        }
        if self.free + need > self.limit() {
            self.stats.allocation_failures += 1;
            return Err(GcError::OutOfMemory {
                requested: need,
                used: self.used_bytes(),
                capacity: self.half,
            });
        }
        let at = self.free;
        self.free += need;
        let h = self.heap.emplace(at, nrefs, ndata);
        Ok(self.account(h))
    }

    fn write_field(&mut self, obj: Handle, i: u16, val: Handle) {
        self.heap.set_field(obj, i, val);
    }

    fn collect(&mut self) {
        let start = Instant::now();
        let before = self.used_bytes();
        let before_objects = self.live_objects;
        let copied_before = self.stats.objects_copied;

        // Swap the halves. Everything the mutator built is now in from-space,
        // and to-space is empty.
        std::mem::swap(&mut self.current, &mut self.idle);
        self.free = self.current;
        self.scan = self.current;

        // Copying a root both moves the object and gives us the new address to
        // store back into the root itself.
        let mut roots = std::mem::take(&mut self.roots);
        roots.for_each_mut(|h| *h = self.copy_object(*h));
        self.roots = roots;

        self.scan_to_space();

        // From-space is abandoned wholesale. Nothing in it is examined, and the
        // forwarding notes left in it are overwritten the next time it is used.
        let survivors = self.stats.objects_copied - copied_before;
        self.live_objects = survivors;
        self.stats.objects_freed += before_objects - survivors;
        self.stats.bytes_freed += before.saturating_sub(self.used_bytes()) as u64;
        if self.used_bytes() == 0 {
            self.collections_with_no_survivors += 1;
        }
        self.stats.collections += 1;
        self.stats.major_collections += 1;
        self.stats.record_pause(start.elapsed());
    }
}
