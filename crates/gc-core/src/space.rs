//! A correct free-list heap, shared by the collectors that do not move objects.
//!
//! Module 1 is where allocation itself is the subject; from module 2 onwards it
//! is scaffolding, so this space is provided working. It hands out memory from
//! a bump pointer until that runs out, then recycles blocks that have been
//! handed back.
//!
//! Everything in `[0, bump)` carries a valid header, free blocks included, so
//! the region can always be walked. Sweeping collectors depend on that.

use crate::heap::{ALIGN, FLAG_FREE, HEADER_SIZE, Handle, Heap};

pub struct FreeListSpace {
    heap: Heap,
    /// Free block addresses. Kept unsorted for cheap frees; sorted on demand.
    free: Vec<Handle>,
    sorted: bool,
    bump: u32,
    used: u32,
}

impl FreeListSpace {
    pub fn new(capacity: usize) -> FreeListSpace {
        FreeListSpace {
            heap: Heap::new(capacity),
            free: Vec::new(),
            sorted: true,
            bump: 0,
            used: 0,
        }
    }

    pub fn heap(&self) -> &Heap {
        &self.heap
    }
    pub fn heap_mut(&mut self) -> &mut Heap {
        &mut self.heap
    }
    pub fn capacity(&self) -> u32 {
        self.heap.capacity()
    }
    /// Bytes committed to live objects.
    pub fn used_bytes(&self) -> u32 {
        self.used
    }
    /// Bytes held in free blocks, not counting memory past the bump pointer.
    pub fn free_bytes(&self) -> u32 {
        self.free.iter().map(|&b| self.heap.size(b)).sum()
    }
    /// First address never yet handed out; the heap walk ends here.
    pub fn bump_pointer(&self) -> u32 {
        self.bump
    }
    pub fn free_block_count(&self) -> usize {
        self.free.len()
    }
    pub fn largest_free_block(&self) -> u32 {
        self.free
            .iter()
            .map(|&b| self.heap.size(b))
            .max()
            .unwrap_or(0)
    }
    pub fn is_free(&self, h: Handle) -> bool {
        self.heap.flag(h, FLAG_FREE)
    }

    /// Allocate an object, or `None` if no block is large enough.
    ///
    /// Coalescing is not attempted here: a caller that gets `None` should
    /// collect, or call [`FreeListSpace::coalesce`], and try again.
    pub fn alloc(&mut self, nrefs: u16, ndata: u32) -> Option<Handle> {
        let need = Heap::size_for(nrefs, ndata);
        let at = match self.take_free(need) {
            Some(at) => at,
            None => {
                if self.bump + need > self.capacity() {
                    return None;
                }
                let at = self.bump;
                self.bump += need;
                at
            }
        };
        self.used += need;
        Some(self.heap.emplace(at, nrefs, ndata))
    }

    fn take_free(&mut self, need: u32) -> Option<u32> {
        let idx = self.free.iter().position(|&b| self.heap.size(b) >= need)?;
        let block = self.free.swap_remove(idx);
        self.sorted = false;
        let remainder = self.heap.size(block) - need;
        if remainder > 0 {
            let split = self.heap.emplace_block(block.0 + need, remainder);
            self.free.push(split);
        }
        Some(block.0)
    }

    /// Return a live object's memory to the free list.
    ///
    /// The payload is poisoned and the block's reference count is cleared, so a
    /// dangling handle reads obvious rubbish rather than plausible stale data.
    pub fn free(&mut self, h: Handle) {
        debug_assert!(!self.is_free(h), "{h:?} freed twice");
        let size = self.heap.size(h);
        self.heap.poison(h);
        self.heap.set_nrefs(h, 0);
        self.heap.set_rc(h, 0);
        self.heap.set_marked(h, false);
        self.used -= size;
        self.free.push(h);
        self.sorted = false;
    }

    /// Merge adjacent free blocks, so that small holes become usable ones.
    pub fn coalesce(&mut self) -> u64 {
        self.sort_free();
        let mut merged: Vec<Handle> = Vec::with_capacity(self.free.len());
        let mut merges = 0;
        for &block in &self.free {
            match merged.last().copied() {
                Some(prev) if prev.0 + self.heap.size(prev) == block.0 => {
                    let combined = self.heap.size(prev) + self.heap.size(block);
                    self.heap.emplace_block(prev.0, combined);
                    merges += 1;
                }
                _ => merged.push(block),
            }
        }
        self.free = merged;
        self.sorted = true;

        // A free run that reaches the bump pointer is better given back as
        // virgin memory: it can then satisfy a request of any size.
        if let Some(&last) = self.free.last()
            && last.0 + self.heap.size(last) == self.bump
        {
            self.free.pop();
            self.bump = last.0;
        }
        merges
    }

    fn sort_free(&mut self) {
        if !self.sorted {
            self.free.sort();
            self.sorted = true;
        }
    }

    /// Free blocks, in address order.
    pub fn free_blocks(&mut self) -> &[Handle] {
        self.sort_free();
        &self.free
    }

    /// Every block in the heap, live or free, in address order.
    pub fn walk(&self) -> crate::heap::Walk<'_> {
        self.heap.walk(0, self.bump)
    }

    /// Every live block, in address order.
    pub fn live_blocks(&self) -> Vec<Handle> {
        self.walk().filter(|&h| !self.is_free(h)).collect()
    }

    /// Total size of every live block, computed by walking rather than from
    /// the space's own bookkeeping.
    pub fn walk_live_bytes(&self) -> u32 {
        self.walk()
            .filter(|&h| !self.is_free(h))
            .map(|h| self.heap.size(h))
            .sum()
    }

    /// Check that the heap is walkable and the free list agrees with it.
    pub fn validate(&mut self) -> Result<(), String> {
        let mut walked: Vec<Handle> = Vec::new();
        let mut at = 0u32;
        while at < self.bump {
            let h = Handle(at);
            let size = self.heap.size(h);
            if size < HEADER_SIZE || !size.is_multiple_of(ALIGN) || at + size > self.bump {
                return Err(format!(
                    "block at {at:#x} claims size {size}; the walk cannot continue"
                ));
            }
            if self.is_free(h) {
                walked.push(h);
            }
            at += size;
        }
        self.sort_free();
        if self.free != walked {
            return Err(format!(
                "the free list holds {:?} but walking the heap finds {:?}",
                self.free, walked
            ));
        }
        if self.used != self.walk_live_bytes() {
            return Err(format!(
                "used_bytes() is {} but the walk finds {} live bytes",
                self.used,
                self.walk_live_bytes()
            ));
        }
        Ok(())
    }
}
