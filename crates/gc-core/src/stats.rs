//! Counters every collector maintains, and which the benchmark reports.
//!
//! Keeping these honest is part of each exercise: a collector that reclaims
//! memory but never records `bytes_freed` will pass a liveness test and fail
//! an accounting one.

use std::time::Duration;

#[derive(Default, Clone, Debug)]
pub struct GcStats {
    /// Objects handed out by `alloc`.
    pub allocations: u64,
    /// Bytes handed out by `alloc`, including headers and padding.
    pub bytes_allocated: u64,
    /// Allocation requests that could not be satisfied even after collecting.
    pub allocation_failures: u64,

    /// Total collections of any kind.
    pub collections: u64,
    /// Collections that looked at only the youngest generation.
    pub minor_collections: u64,
    /// Collections that looked at the whole heap.
    pub major_collections: u64,

    /// Objects reclaimed across all collections.
    pub objects_freed: u64,
    /// Bytes reclaimed across all collections.
    pub bytes_freed: u64,
    /// Objects visited by a marking phase.
    pub objects_marked: u64,
    /// Objects physically moved by a relocating collector.
    pub objects_copied: u64,
    /// Objects moved from a younger generation to an older one.
    pub objects_promoted: u64,

    /// Write barrier invocations.
    pub barrier_hits: u64,
    /// Entries currently in the remembered set or card table.
    pub remembered_entries: u64,
    /// Incremental marking steps performed.
    pub increments: u64,

    /// High water mark of bytes in use.
    pub peak_used_bytes: u64,
    /// Time spent inside collections.
    pub gc_time: Duration,
    /// Longest single stop-the-world pause.
    pub max_pause: Duration,
}

impl GcStats {
    /// Fold one collection's duration into the timing counters.
    pub fn record_pause(&mut self, d: Duration) {
        self.gc_time += d;
        if d > self.max_pause {
            self.max_pause = d;
        }
    }

    /// Update the high water mark.
    pub fn observe_used(&mut self, used: u32) {
        if used as u64 > self.peak_used_bytes {
            self.peak_used_bytes = used as u64;
        }
    }
}
