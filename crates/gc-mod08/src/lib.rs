//! # Module 8 — Incremental tri-colour marking
//!
//! Every collector so far has stopped the program dead, done all of its work,
//! and let the program continue. For most of this course that was invisible,
//! because the heaps were small. It stops being invisible at scale: a
//! stop-the-world mark of a ten gigabyte heap is a ten gigabyte pause, and no
//! amount of throughput makes up for a program that stops responding for a
//! second at a time.
//!
//! This module marks a little at a time. The mutator runs in between the
//! increments, which means the object graph is being rewritten underneath the
//! marker while it works. That is a genuinely harder problem than anything so
//! far, and the tri-colour abstraction is what makes it tractable.
//!
//! ## Three colours
//!
//! Every object is exactly one of:
//!
//! - **white** — not yet reached. At the end of the cycle, white means garbage.
//! - **grey** — reached, but its own references have not been looked at yet.
//! - **black** — reached, and its references have been looked at.
//!
//! Marking starts by shading the roots grey and ends when no grey objects are
//! left. The grey set is exactly the marker's worklist, and "no grey objects
//! remain" is exactly "the wavefront has swept the whole reachable graph".
//!
//! ## The invariant
//!
//! The thing that must stay true is this:
//!
//! > **No black object holds a reference to a white object.**
//!
//! Black means *finished*: the marker will never look at that object again. So
//! a reference from black to white is a reachable object the marker has already
//! promised not to visit, and the sweep will free it while the program is still
//! using it.
//!
//! While the collector alone is running, the invariant holds for free. The
//! mutator is what breaks it, and it can do so with a single ordinary store:
//!
//! ```text
//!     black B ────────► white W          B.f = W
//!     (already scanned)  (never reached)
//! ```
//!
//! ## The barrier restores it
//!
//! Every reference store runs collector code first, and there is more than one
//! correct thing for it to do:
//!
//! - **Dijkstra** shades the value *being stored*. If W is grey, it is on the
//!   worklist, and black-to-grey is not a violation.
//! - **Yuasa** shades the value *being overwritten*, preserving the graph as it
//!   was when the cycle began.
//!
//! They protect different things and are not interchangeable: this collector
//! marks by *incremental update*, discovering new edges as they appear, so it
//! needs the new value shaded. Shading only the old value leaves the new one
//! white, and the sweep takes it.
//!
//! ## Objects born mid-cycle
//!
//! An object allocated while marking is in progress was never reachable when
//! the roots were shaded, so the marker will not find it. Left white, it is
//! freed the moment it is born. It is therefore allocated **black**: assumed
//! live for this cycle, and considered properly by the next one.
//!
//! ## Floating garbage
//!
//! The price of all this is precision. An object marked early in the cycle and
//! abandoned a moment later is still black when the sweep runs, so it survives
//! a cycle it spent entirely dead. This is **floating garbage**, and it is not
//! a bug — it is the cost of not stopping the world. It is collected by the
//! next cycle.
//!
//! ## Your work
//!
//! [`Incremental::mark_step`] is unimplemented. The rest of the module is
//! written but does not pass its tests.

use std::time::Instant;

use gc_core::collector::{Collector, GcError};
use gc_core::heap::{BLACK, GREY, Handle, Heap, WHITE};
use gc_core::roots::Roots;
use gc_core::space::FreeListSpace;
use gc_core::stats::GcStats;

/// Start a cycle once this many bytes are in use.
const MIN_THRESHOLD: u32 = 64 << 10;

/// Objects blackened per allocation while a cycle is in progress. Small enough
/// that no single allocation does much work; large enough that marking finishes
/// before the heap fills.
const MARK_BUDGET: usize = 8;

/// Objects blackened per step when finishing a cycle in one go.
const DRAIN_BUDGET: usize = 64;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Phase {
    /// No cycle in progress. Every object is white.
    Idle,
    /// A cycle is in progress and the grey set is the wavefront.
    Marking,
}

pub struct Incremental {
    space: FreeListSpace,
    roots: Roots,
    stats: GcStats,
    phase: Phase,
    /// Reached but not yet scanned. This is the grey set.
    grey: Vec<Handle>,
    /// Bytes in use that will start the next cycle.
    threshold: u32,
}

impl Incremental {
    pub fn new(capacity: usize) -> Incremental {
        Incremental {
            space: FreeListSpace::new(capacity),
            roots: Roots::new(),
            stats: GcStats::default(),
            phase: Phase::Idle,
            grey: Vec::new(),
            threshold: MIN_THRESHOLD.min(capacity as u32 / 2),
        }
    }

    pub fn space(&self) -> &FreeListSpace {
        &self.space
    }
    /// Whether a marking cycle is in progress.
    pub fn phase(&self) -> Phase {
        self.phase
    }
    /// Objects reached but not yet scanned.
    pub fn grey_count(&self) -> usize {
        self.grey.len()
    }
    /// The colour of `h`: [`WHITE`], [`GREY`] or [`BLACK`].
    pub fn color_of(&self, h: Handle) -> u8 {
        self.space.heap().color(h)
    }
    /// True if the marker has finished with `h` and will not return to it.
    pub fn is_black(&self, h: Handle) -> bool {
        self.color_of(h) == BLACK
    }
    pub fn is_free(&self, h: Handle) -> bool {
        self.space.is_free(h)
    }
    /// Bytes in use that will start the next cycle.
    pub fn threshold(&self) -> u32 {
        self.threshold
    }
    /// Every live object, in address order.
    pub fn objects(&self) -> Vec<Handle> {
        self.space
            .heap()
            .walk(0, self.space.bump_pointer())
            .filter(|&h| !self.space.is_free(h))
            .collect()
    }

    /// Shade `h` grey if it has not been reached yet.
    ///
    /// Shading an object that is already grey or black must do nothing, or the
    /// same object is scanned repeatedly and marking may never terminate.
    fn shade(&mut self, h: Handle) {
        if self.space.heap().color(h) == WHITE {
            self.space.heap_mut().set_color(h, GREY);
            self.grey.push(h);
        }
    }

    /// Begin a cycle: shade the roots and leave everything else white.
    pub fn start_cycle(&mut self) {
        self.grey.clear();
        let roots: Vec<Handle> = self.roots.iter().collect();
        for r in roots {
            self.shade(r);
        }
        self.phase = Phase::Marking;
    }

    /// Blacken up to `budget` grey objects.
    ///
    /// Take a grey object, shade everything it refers to, and colour it black —
    /// black meaning the marker is finished with it and will not come back.
    /// Stop when the budget is spent or nothing is grey, whichever comes first,
    /// so that the mutator gets to run again promptly.
    ///
    /// Count objects blackened in [`gc_core::stats::GcStats::objects_marked`]
    /// and each call in `increments`.
    pub fn mark_step(&mut self, budget: usize) {
        todo!("blacken up to {budget} grey objects, shading what they refer to")
    }

    /// Drive marking to completion.
    ///
    /// The mutator has been running throughout the cycle, so the root set is
    /// not the one shaded at the start: locals have been pushed and popped and
    /// globals rebound. The roots are shaded again here before the grey set is
    /// drained.
    pub fn finish_marking(&mut self) {
        let roots: Vec<Handle> = self.roots.iter().collect();
        for r in roots {
            self.shade(r);
        }
        while !self.grey.is_empty() {
            self.mark_step(DRAIN_BUDGET);
        }
    }

    /// Free every white object and return the survivors to white.
    ///
    /// A colour belongs to the cycle that assigned it. Anything still black
    /// when the next cycle starts would be treated as already scanned.
    pub fn sweep(&mut self) {
        let bump = self.space.bump_pointer();
        let mut doomed: Vec<Handle> = Vec::new();
        let mut at = 0u32;
        while at < bump {
            let h = Handle(at);
            at += self.space.heap().size(h);
            if self.space.is_free(h) {
                continue;
            }
            if self.space.heap().color(h) == WHITE {
                doomed.push(h);
            } else {
                self.space.heap_mut().set_color(h, WHITE);
            }
        }

        for h in doomed {
            let size = self.space.heap().size(h);
            self.space.free(h);
            self.stats.objects_freed += 1;
            self.stats.bytes_freed += size as u64;
        }
        self.space.coalesce();

        self.phase = Phase::Idle;
        let live = self.space.used_bytes();
        self.threshold = live
            .saturating_mul(2)
            .max(MIN_THRESHOLD)
            .min(self.space.capacity());
    }

    /// Finish whatever cycle is in progress, starting one if necessary.
    fn complete_cycle(&mut self) {
        let start = Instant::now();
        if self.phase == Phase::Idle {
            self.start_cycle();
        }
        self.finish_marking();
        self.sweep();
        self.stats.collections += 1;
        self.stats.major_collections += 1;
        self.stats.record_pause(start.elapsed());
    }

    fn account(&mut self, h: Handle) -> Handle {
        self.stats.allocations += 1;
        self.stats.bytes_allocated += self.space.heap().size(h) as u64;
        self.stats.observe_used(self.space.used_bytes());
        h
    }
}

impl Collector for Incremental {
    fn name(&self) -> &'static str {
        "incremental tri-colour"
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
        // Allocation is what drives marking forward: the program pays for its
        // own collection, a little at each allocation, rather than all at once.
        match self.phase {
            Phase::Marking => {
                // Each increment is a pause of its own, and recording them is
                // how the benchmark can show that they are small.
                let start = Instant::now();
                self.mark_step(MARK_BUDGET);
                if self.grey.is_empty() {
                    self.finish_marking();
                    self.sweep();
                    self.stats.collections += 1;
                    self.stats.major_collections += 1;
                }
                self.stats.record_pause(start.elapsed());
            }
            Phase::Idle => {
                if self.space.used_bytes() >= self.threshold {
                    self.start_cycle();
                }
            }
        }

        let h = match self.space.alloc(nrefs, ndata) {
            Some(h) => h,
            None => {
                self.complete_cycle();
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

        // Nothing has reached this object, which is precisely what white
        // means, and the next cycle will consider it from the roots.
        self.space.heap_mut().set_color(h, WHITE);
        Ok(self.account(h))
    }

    /// The write barrier.
    ///
    /// While a cycle is in progress an ordinary store can point a finished
    /// object at one the marker has never seen, which is the one thing the
    /// invariant forbids.
    fn write_field(&mut self, obj: Handle, i: u16, val: Handle) {
        self.stats.barrier_hits += 1;
        if self.phase == Phase::Marking && !val.is_null() {
            // An object the marker has not finished with will have its
            // references looked at when its turn comes, so only stores into
            // one it has not reached yet need anything doing about them.
            if self.color_of(obj) != BLACK {
                self.shade(val);
            }
        }
        self.space.heap_mut().set_field(obj, i, val);
    }

    fn collect(&mut self) {
        self.complete_cycle();
    }
}
