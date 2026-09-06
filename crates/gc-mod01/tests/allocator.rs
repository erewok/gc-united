//! What module 1's allocator promises.
//!
//! These tests are the specification. Nothing else describes the intended
//! behaviour, so read a failure as a statement about the heap, not as a
//! pointer to a line of code.

use gc_core::heap::{ALIGN, FLAG_FREE, HEADER_SIZE, Handle, Heap};
use gc_mod01::{Allocator, FitPolicy};

/// A plain 64-byte cell: no reference slots, 48 bytes of payload.
const CELL_REFS: u16 = 0;
const CELL_DATA: u32 = 48;
const CELL_SIZE: u32 = 64;

fn cell(a: &mut Allocator) -> Handle {
    a.alloc(CELL_REFS, CELL_DATA)
        .expect("the heap should have had room for a 64 byte cell")
}

/// Allocate `n` cells, failing the test if any request is refused.
fn cells(a: &mut Allocator, n: usize) -> Vec<Handle> {
    (0..n).map(|_| cell(a)).collect()
}

#[test]
fn cell_size_is_what_the_tests_assume() {
    assert_eq!(Heap::size_for(CELL_REFS, CELL_DATA), CELL_SIZE);
}

#[test]
fn allocations_are_aligned_distinct_and_in_bounds() {
    let mut a = Allocator::new(64 << 10);
    let mut seen: Vec<(u32, u32)> = Vec::new();

    for i in 0..200 {
        let h = a
            .alloc((i % 4) as u16, 8 + (i % 5) * 16)
            .expect("room for 200 small objects");
        let size = a.heap().size(h);

        assert_eq!(
            h.addr() % ALIGN,
            0,
            "object {i} was placed at unaligned address {h:?}"
        );
        assert!(size >= HEADER_SIZE, "object {i} at {h:?} has size {size}");
        assert!(
            h.addr() + size <= a.capacity(),
            "object {i} at {h:?} extends past the end of the heap"
        );

        for &(start, len) in &seen {
            let overlaps = h.addr() < start + len && start < h.addr() + size;
            assert!(
                !overlaps,
                "object {i} at {h:?} (size {size}) overlaps an earlier object at {start:#x} \
                 (size {len})"
            );
        }
        seen.push((h.addr(), size));
    }
    a.validate().expect("heap should still be walkable");
}

#[test]
fn payloads_are_independent() {
    let mut a = Allocator::new(64 << 10);
    let hs = cells(&mut a, 100);

    for (i, &h) in hs.iter().enumerate() {
        a.heap_mut().set_data_u64(h, 0, 0xA5A5_0000 + i as u64);
        a.heap_mut().set_data_u64(h, 8, !(i as u64));
    }
    for (i, &h) in hs.iter().enumerate() {
        assert_eq!(
            a.heap().data_u64(h, 0),
            0xA5A5_0000 + i as u64,
            "object {i} at {h:?} does not hold what was written to it"
        );
        assert_eq!(
            a.heap().data_u64(h, 8),
            !(i as u64),
            "object {i}: second word differs"
        );
    }
}

#[test]
fn the_heap_can_be_filled_completely() {
    // Exactly 64 cells fit, with nothing left over.
    let capacity = (CELL_SIZE * 64) as usize;
    let mut a = Allocator::new(capacity);

    for i in 0..64 {
        assert!(
            a.alloc(CELL_REFS, CELL_DATA).is_some(),
            "cell {i} of 64 was refused, but {} of {capacity} bytes are still unused",
            capacity as u32 - a.used_bytes()
        );
    }

    assert_eq!(
        a.used_bytes(),
        capacity as u32,
        "64 cells of {CELL_SIZE} bytes should exactly fill a {capacity} byte heap"
    );
    assert!(
        a.alloc(CELL_REFS, CELL_DATA).is_none(),
        "a 65th cell should not fit"
    );
    a.validate()
        .expect("a completely full heap should still be walkable");
}

#[test]
fn freed_space_is_reused_rather_than_growing_the_heap() {
    let mut a = Allocator::new(16 << 10);

    let first = cells(&mut a, 64);
    let high_water = a.bump_pointer();
    for h in first {
        a.free(h);
    }

    let _ = cells(&mut a, 64);
    assert_eq!(
        a.bump_pointer(),
        high_water,
        "64 cells were freed and 64 identical cells allocated, but fresh memory was taken \
         from beyond {high_water:#x} instead of reusing them"
    );
    a.validate().expect("heap should still be walkable");
}

#[test]
fn an_exactly_sized_hole_is_reused() {
    let mut a = Allocator::new(16 << 10);
    let _before = cell(&mut a);
    let middle = cell(&mut a);
    let _after = cell(&mut a);

    let hole = middle.addr();
    a.free(middle);
    assert_eq!(
        a.free_block_count(),
        1,
        "freeing one object should leave exactly one hole"
    );
    assert_eq!(
        a.heap().size(Handle(hole)),
        CELL_SIZE,
        "the hole should be cell-sized"
    );

    let replacement = cell(&mut a);
    assert_eq!(
        replacement.addr(),
        hole,
        "a {CELL_SIZE} byte hole was available at {hole:#x} and a {CELL_SIZE} byte object was \
         requested, but it was placed at {:#x} instead",
        replacement.addr()
    );
    assert_eq!(
        a.free_block_count(),
        0,
        "the hole was exactly filled, so no free block remains"
    );
}

#[test]
fn used_bytes_matches_the_heap_walk() {
    let mut a = Allocator::new(32 << 10);
    let mut live: Vec<Handle> = Vec::new();

    for round in 0..300u32 {
        if round % 3 == 2 && !live.is_empty() {
            let h = live.remove((round as usize * 7) % live.len());
            a.free(h);
        } else {
            live.push(
                a.alloc((round % 3) as u16, 8 + (round % 7) * 8)
                    .expect("room"),
            );
        }

        assert_eq!(
            a.used_bytes(),
            a.walk_live_bytes(),
            "after round {round}, used_bytes() reports {} but walking the heap finds {} bytes \
             in blocks that are not marked free",
            a.used_bytes(),
            a.walk_live_bytes()
        );
        assert_eq!(
            a.used_bytes() + a.free_bytes() + a.virgin_bytes(),
            a.capacity(),
            "after round {round}, live ({}) + free ({}) + never-used ({}) should account for \
             the whole {} byte heap",
            a.used_bytes(),
            a.free_bytes(),
            a.virgin_bytes(),
            a.capacity()
        );
    }
}

#[test]
fn coalescing_merges_every_adjacent_hole() {
    let mut a = Allocator::new(16 << 10);
    let hs = cells(&mut a, 64);
    let spanned = a.bump_pointer();

    for h in hs {
        a.free(h);
    }
    assert_eq!(
        a.free_block_count(),
        64,
        "each freed cell should start as its own hole"
    );

    a.coalesce_all();
    a.validate()
        .expect("coalescing must leave the heap walkable");

    assert_eq!(
        a.free_block_count(),
        1,
        "64 cells were freed and they were laid out back to back, so they should merge into \
         a single hole; the free list still holds {} blocks",
        a.free_block_count()
    );
    assert_eq!(
        a.largest_free_block(),
        spanned,
        "the merged hole should span all {spanned} bytes that were freed"
    );

    let big = a
        .alloc(0, spanned - HEADER_SIZE)
        .expect("after merging, one object should be able to occupy the whole reclaimed region");
    assert_eq!(
        big.addr(),
        0,
        "the merged hole starts at the bottom of the heap"
    );
}

#[test]
fn coalescing_leaves_non_adjacent_holes_alone() {
    let mut a = Allocator::new(16 << 10);
    let hs = cells(&mut a, 32);

    // Free every other cell, so no two holes touch.
    for (i, &h) in hs.iter().enumerate() {
        if i % 2 == 0 {
            a.free(h);
        }
    }
    let before = a.free_block_count();
    assert_eq!(before, 16, "every other cell of 32 was freed");

    a.coalesce_all();
    a.validate()
        .expect("coalescing must leave the heap walkable");
    assert_eq!(
        a.free_block_count(),
        16,
        "none of these holes are adjacent, so none of them should have merged"
    );
    assert_eq!(
        a.largest_free_block(),
        CELL_SIZE,
        "each hole is still one cell wide"
    );

    // Now free the rest; everything should become one block.
    for (i, &h) in hs.iter().enumerate() {
        if i % 2 == 1 {
            a.free(h);
        }
    }
    a.coalesce_all();
    a.validate()
        .expect("coalescing must leave the heap walkable");
    assert_eq!(
        a.free_block_count(),
        1,
        "with every cell freed, one hole should remain"
    );
}

#[test]
fn best_fit_takes_the_tightest_hole() {
    // Lay out live cells separating three holes of 192, 64 and 128 bytes,
    // in that order, then ask for 64.
    let layout = |a: &mut Allocator| -> (u32, u32, u32) {
        let _guard0 = cell(a);
        let big = a.alloc(0, 192 - HEADER_SIZE).unwrap();
        let _guard1 = cell(a);
        let exact = cell(a);
        let _guard2 = cell(a);
        let medium = a.alloc(0, 128 - HEADER_SIZE).unwrap();
        let _guard3 = cell(a);

        let (b, e, m) = (big.addr(), exact.addr(), medium.addr());
        a.free(big);
        a.free(exact);
        a.free(medium);
        (b, e, m)
    };

    let mut first = Allocator::with_policy(16 << 10, FitPolicy::First);
    let (big, _exact, _medium) = layout(&mut first);
    let got = cell(&mut first);
    assert_eq!(
        got.addr(),
        big,
        "first fit should take the earliest hole that is big enough, at {big:#x}"
    );

    let mut best = Allocator::with_policy(16 << 10, FitPolicy::Best);
    let (big, exact, medium) = layout(&mut best);
    assert_eq!(
        best.free_block_count(),
        3,
        "three holes should be available"
    );
    let got = cell(&mut best);
    assert_eq!(
        got.addr(),
        exact,
        "holes of 192 bytes (at {big:#x}), 64 bytes (at {exact:#x}) and 128 bytes (at \
         {medium:#x}) were available for a {CELL_SIZE} byte request, but it went to {:#x}",
        got.addr()
    );
    assert_eq!(
        best.free_block_count(),
        2,
        "the 64 byte hole was filled exactly, so it should not have been split"
    );
}

#[test]
fn fragmentation_is_measurable() {
    // Exactly full, so that a failed allocation means "no hole fits" rather
    // than "the bump pointer ran out".
    let mut a = Allocator::new((CELL_SIZE * 64) as usize);
    let hs = cells(&mut a, 64);
    assert_eq!(a.virgin_bytes(), 0, "the heap should be exactly full");
    for (i, &h) in hs.iter().enumerate() {
        if i % 2 == 0 {
            a.free(h);
        }
    }

    let free = a.free_bytes();
    let largest = a.largest_free_block();
    assert!(
        free > largest,
        "the heap holds {free} free bytes in blocks of at most {largest}"
    );
    assert!(
        a.alloc(0, free - HEADER_SIZE).is_none(),
        "an object needing all {free} free bytes cannot fit in any single hole"
    );
}

#[test]
fn random_churn_keeps_the_heap_consistent() {
    let mut a = Allocator::new(64 << 10);
    let mut live: Vec<Handle> = Vec::new();
    let mut rng: u64 = 0x5EED;
    let mut next = || {
        rng ^= rng << 13;
        rng ^= rng >> 7;
        rng ^= rng << 17;
        rng
    };

    for round in 0..4000u32 {
        let free_it = !live.is_empty() && next() % 100 < 45;
        if free_it {
            let idx = (next() % live.len() as u64) as usize;
            let h = live.swap_remove(idx);
            a.free(h);
        } else {
            let nrefs = (next() % 5) as u16;
            let ndata = 8 + (next() % 12) as u32 * 16;
            match a.alloc(nrefs, ndata) {
                Some(h) => live.push(h),
                None => {
                    a.coalesce_all();
                    if let Some(h) = a.alloc(nrefs, ndata) {
                        live.push(h);
                    }
                }
            }
        }

        if round % 200 == 0 {
            a.validate()
                .unwrap_or_else(|e| panic!("after round {round}: {e}"));
        }
    }

    a.validate().expect("heap should be walkable at the end");
    assert_eq!(
        a.used_bytes(),
        a.walk_live_bytes(),
        "final accounting disagrees with the walk"
    );

    for h in live {
        a.free(h);
    }
    a.coalesce_all();
    a.validate()
        .expect("heap should be walkable once everything is freed");
    assert_eq!(
        a.used_bytes(),
        0,
        "every object was freed, so nothing should be in use"
    );
    assert_eq!(
        a.free_block_count(),
        1,
        "with the whole heap free and coalesced, one block should remain, not {}",
        a.free_block_count()
    );
}

#[test]
fn freed_memory_is_poisoned() {
    let mut a = Allocator::new(16 << 10);
    let h = cell(&mut a);
    a.heap_mut().set_data_u64(h, 0, 0x1234_5678_9ABC_DEF0);
    a.free(h);

    assert!(
        a.heap().flag(h, FLAG_FREE),
        "a freed block should be flagged free"
    );
    assert!(
        a.heap().is_poisoned(h),
        "a freed block's payload should be overwritten, so that code still holding the old \
         address reads obvious rubbish instead of plausible stale data"
    );
}
