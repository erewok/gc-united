//! # Module 1 — Allocation
//!
//! Before anything can be collected, something has to hand out memory. This
//! module is the substrate every later module is built on: an explicit-free
//! allocator with no garbage collection at all. You call [`Allocator::free`]
//! yourself, and if you forget, the memory is gone for good.
//!
//! Two mechanisms live here, and real allocators use both.
//!
//! A **bump pointer** hands out never-before-used memory by adding to a
//! pointer. It is the fastest possible allocator — two instructions — but it
//! can only move forwards, so on its own it never reuses anything.
//!
//! A **free list** tracks memory that has been handed back. Allocating from it
//! means searching for a block big enough, and usually splitting the block that
//! is found. Blocks are recycled, but the search costs time and the splitting
//! costs space: a heap can end up with plenty of free bytes and no single hole
//! big enough for the next request. That is *external fragmentation*, and it is
//! the problem every compacting collector in later modules exists to solve.
//!
//! The two are complementary, and this allocator uses both: the free list
//! first, so memory gets reused, and the bump pointer for anything the free
//! list cannot satisfy.
//!
//! ## The heap walk
//!
//! Every block in `[0, bump)` — allocated or free — carries its own size in its
//! header. That makes the region walkable: start at zero, read a size, jump
//! that far, repeat. [`Allocator::validate`] does exactly this, and so does
//! every sweeping collector from module 4 onwards. Any code that breaks the
//! walk breaks everything downstream, so it is worth keeping the invariant in
//! mind: *a block's header always tells the truth about how big it is.*
//!
//! ## Your work
//!
//! [`Allocator::best_fit`] and [`Allocator::coalesce_all`] are unimplemented.
//! The rest of the module is written but does not pass its tests.

use gc_core::heap::{FLAG_FREE, HEADER_SIZE, Handle, Heap};

/// How the allocator chooses among free blocks that are big enough.
#[derive(Copy, Clone, PartialEq, Eq, Debug, Default)]
pub enum FitPolicy {
    /// Take the first block that fits. Fast, but tends to chew up the large
    /// blocks near the start of the heap.
    #[default]
    First,
    /// Take the smallest block that fits, leaving larger ones intact for
    /// larger requests. Slower, and leaves behind more tiny unusable holes.
    Best,
}

#[derive(Default, Clone, Debug)]
pub struct AllocStats {
    pub allocations: u64,
    pub frees: u64,
    pub bytes_allocated: u64,
    pub bytes_freed: u64,
    /// Free blocks split to satisfy a smaller request.
    pub splits: u64,
    /// Adjacent free blocks merged into one.
    pub merges: u64,
    /// Requests that could not be satisfied.
    pub failures: u64,
    /// Blocks taken from the free list rather than from fresh memory.
    pub reused: u64,
}

pub struct Allocator {
    heap: Heap,
    /// Free block addresses, kept sorted so that adjacency is easy to see.
    free_handles: Vec<Handle>,
    /// First address never yet handed out. `[bump, capacity)` is virgin memory
    /// and has no headers, so the heap walk stops here.
    bump: u32,
    used: u32,
    policy: FitPolicy,
    stats: AllocStats,
}

impl Allocator {
    pub fn new(capacity: usize) -> Allocator {
        Allocator {
            heap: Heap::new(capacity),
            free_handles: Vec::new(),
            bump: 0,
            used: 0,
            policy: FitPolicy::First,
            stats: AllocStats::default(),
        }
    }

    pub fn with_policy(capacity: usize, policy: FitPolicy) -> Allocator {
        Allocator {
            policy,
            ..Allocator::new(capacity)
        }
    }

    pub fn heap(&self) -> &Heap {
        &self.heap
    }
    pub fn heap_mut(&mut self) -> &mut Heap {
        &mut self.heap
    }
    pub fn stats(&self) -> &AllocStats {
        &self.stats
    }
    pub fn policy(&self) -> FitPolicy {
        self.policy
    }
    pub fn capacity(&self) -> u32 {
        self.heap.capacity()
    }
    /// First address never yet handed out.
    pub fn bump_pointer(&self) -> u32 {
        self.bump
    }
    /// Bytes currently committed to live objects.
    pub fn used_bytes(&self) -> u32 {
        self.used
    }
    /// Bytes sitting in free blocks, not counting virgin memory.
    pub fn free_bytes(&self) -> u32 {
        self.free_handles.iter().map(|&b| self.heap.size(b)).sum()
    }
    /// Bytes past the bump pointer that have never been used.
    pub fn virgin_bytes(&self) -> u32 {
        self.capacity() - self.bump
    }
    pub fn free_block_count(&self) -> usize {
        self.free_handles.len()
    }
    /// Size of the largest single free block, ignoring virgin memory.
    ///
    /// The gap between this and [`Allocator::free_bytes`] is a direct measure
    /// of how fragmented the heap has become.
    pub fn largest_free_block(&self) -> u32 {
        self.free_handles
            .iter()
            .map(|&b| self.heap.size(b))
            .max()
            .unwrap_or(0)
    }
    /// Free block addresses, in address order.
    pub fn free_blocks(&self) -> &[Handle] {
        &self.free_handles
    }

    /// Allocate an object with `nrefs` reference slots and `ndata` payload
    /// bytes, or `None` if no room can be found.
    pub fn alloc(&mut self, nrefs: u16, ndata: u32) -> Option<Handle> {
        let need = Heap::size_for(nrefs, ndata);

        let at = match self.take_from_free_list(need) {
            Some(at) => {
                self.stats.reused += 1;
                at
            }
            None => match self.take_from_virgin(need) {
                Some(at) => at,
                None => {
                    self.stats.failures += 1;
                    return None;
                }
            },
        };

        let h = self.heap.emplace(at, nrefs, ndata);
        self.used += need;
        self.stats.allocations += 1;
        self.stats.bytes_allocated += need as u64;
        Some(h)
    }

    /// Hand back an object. Its memory joins the free list.
    pub fn free(&mut self, h: Handle) {
        assert!(
            !self.heap.flag(h, FLAG_FREE),
            "{h:?} was freed twice; the second free would corrupt the free list"
        );
        let size = self.heap.size(h);

        self.heap.poison(h);
        self.heap.set_nrefs(h, 0);

        self.used -= size;
        self.stats.frees += 1;
        self.stats.bytes_freed += size as u64;

        let pos = self.free_handles.partition_point(|b| b.0 < h.0);
        self.free_handles.insert(pos, h);
    }

    /// Carve `need` bytes off the front of never-used memory.
    fn take_from_virgin(&mut self, need: u32) -> Option<u32> {
        let remaining = self.capacity() - self.bump;
        if need < remaining {
            let at = self.bump;
            self.bump += need;
            Some(at)
        } else {
            None
        }
    }

    /// Find a free block big enough, remove it from the list, and split off
    /// whatever is left over.
    fn take_from_free_list(&mut self, need: u32) -> Option<u32> {
        let idx = match self.policy {
            FitPolicy::First => self.first_fit(need),
            FitPolicy::Best => self.best_fit(need),
        }?;

        let block = self.free_handles.remove(idx);
        let block_size = self.heap.size(block);
        let remainder = block_size - need;

        if remainder > 0 {
            let split = self.heap.emplace_block(block.0 + need, remainder);
            self.free_handles.push(split);
            self.stats.splits += 1;
        }

        Some(block.0)
    }

    /// Whether `block` can satisfy a request for `need` bytes.
    ///
    /// Splitting a block leaves a remainder behind, and that remainder has to
    /// carry a header of its own, so there must be room for both.
    fn fits(&self, block: Handle, need: u32) -> bool {
        let size = self.heap.size(block);
        size > need && size - need >= HEADER_SIZE
    }

    /// Index of the first free block with room for `need` bytes.
    fn first_fit(&self, need: u32) -> Option<usize> {
        self.free_handles.iter().position(|&b| self.fits(b, need))
    }

    /// Index of the *smallest* free block with room for `need` bytes, or
    /// `None` if no block is big enough.
    ///
    /// Best fit leaves large blocks intact for large requests, at the cost of
    /// searching the whole list and of leaving behind smaller offcuts. Ties go
    /// to the lowest address, so that allocation stays deterministic.
    fn best_fit(&self, need: u32) -> Option<usize> {
        todo!("choose the smallest free block that can hold {need} bytes")
    }

    /// Merge every run of adjacent free blocks into a single larger block.
    ///
    /// Without this, a heap that has been filled and emptied is left as a long
    /// row of small holes: the bytes are all free, but no single hole is big
    /// enough for a large object. Afterwards no two free blocks are adjacent,
    /// and the free list is rebuilt to match.
    /// Count one merge in [`AllocStats::merges`] for each pair of blocks joined.
    pub fn coalesce_all(&mut self) {
        todo!("merge adjacent free blocks and rebuild the free list")
    }

    /// Walk the heap and check that it is structurally sound.
    ///
    /// This is a debugging tool, not a test: it checks that the heap can be
    /// walked and that the free list agrees with what the walk finds. It says
    /// nothing about whether the allocator's accounting is right.
    pub fn validate(&self) -> Result<(), String> {
        let mut walked_free: Vec<Handle> = Vec::new();
        let mut at = 0u32;
        let mut blocks = 0u64;

        while at < self.bump {
            let h = Handle(at);
            let size = self.heap.size(h);
            if size < gc_core::heap::HEADER_SIZE || !size.is_multiple_of(gc_core::heap::ALIGN) {
                return Err(format!(
                    "block {blocks} at {at:#x} claims size {size}; the walk cannot continue \
                     (sizes must be a non-zero multiple of {})",
                    gc_core::heap::ALIGN
                ));
            }
            if at + size > self.bump {
                return Err(format!(
                    "block {blocks} at {at:#x} claims size {size}, which runs past the bump \
                     pointer at {:#x}",
                    self.bump
                ));
            }
            if self.heap.flag(h, FLAG_FREE) {
                walked_free.push(h);
            }
            at += size;
            blocks += 1;
        }

        if at != self.bump {
            return Err(format!(
                "walking the heap ended at {at:#x} but the bump pointer is at {:#x}",
                self.bump
            ));
        }

        let mut listed = self.free_handles.clone();
        listed.sort();
        if listed != walked_free {
            return Err(format!(
                "the free list holds {} block(s) {:?} but walking the heap finds {} free \
                 block(s) {:?}",
                listed.len(),
                listed,
                walked_free.len(),
                walked_free
            ));
        }

        for w in self.free_handles.windows(2) {
            if w[0].0 >= w[1].0 {
                return Err(format!("the free list is not in address order: {w:?}"));
            }
        }

        Ok(())
    }

    /// Total size of every non-free block found by walking the heap.
    ///
    /// Independent of the allocator's own bookkeeping, and therefore the thing
    /// to compare that bookkeeping against.
    pub fn walk_live_bytes(&self) -> u32 {
        self.heap
            .walk(0, self.bump)
            .filter(|&h| !self.heap.flag(h, FLAG_FREE))
            .map(|h| self.heap.size(h))
            .sum()
    }

    /// Every live block, in address order.
    pub fn live_blocks(&self) -> Vec<Handle> {
        self.heap
            .walk(0, self.bump)
            .filter(|&h| !self.heap.flag(h, FLAG_FREE))
            .collect()
    }
}
