//! The shared free-list space is scaffolding for modules 2 onwards, so it is
//! tested here rather than left for a module to discover.

use gc_core::heap::{HEADER_SIZE, Handle, Heap};
use gc_core::space::FreeListSpace;

#[test]
fn allocates_distinct_blocks_and_tracks_usage() {
    let mut s = FreeListSpace::new(64 << 10);
    let mut hs = Vec::new();
    for i in 0..300u32 {
        let h = s.alloc((i % 4) as u16, 8 + (i % 6) * 16).expect("room");
        hs.push((h, s.heap().size(h)));
    }
    let expected: u32 = hs.iter().map(|&(_, n)| n).sum();
    assert_eq!(s.used_bytes(), expected);
    assert_eq!(s.walk_live_bytes(), expected);
    s.validate().unwrap();
}

#[test]
fn freed_blocks_are_reused_and_coalesced() {
    let mut s = FreeListSpace::new(16 << 10);
    let hs: Vec<Handle> = (0..64).map(|_| s.alloc(0, 48).unwrap()).collect();
    let high_water = s.bump_pointer();
    assert_eq!(high_water, 64 * 64);

    for h in hs {
        s.free(h);
    }
    assert_eq!(s.used_bytes(), 0);
    s.validate().unwrap();

    let merges = s.coalesce();
    assert_eq!(merges, 63, "64 adjacent holes should merge with 63 joins");
    // The merged run reaches the bump pointer, so it is given back as virgin.
    assert_eq!(s.bump_pointer(), 0);
    assert_eq!(s.free_block_count(), 0);
    s.validate().unwrap();

    let big = s.alloc(0, 4096 - HEADER_SIZE).expect("the whole region is available again");
    assert_eq!(big.addr(), 0);
}

#[test]
fn interior_holes_are_reused_without_growing_the_heap() {
    let mut s = FreeListSpace::new(16 << 10);
    let a = s.alloc(0, 48).unwrap();
    let b = s.alloc(0, 48).unwrap();
    let _c = s.alloc(0, 48).unwrap();
    let high_water = s.bump_pointer();

    s.free(b);
    let replacement = s.alloc(0, 48).unwrap();
    assert_eq!(replacement.addr(), b.addr(), "the hole should be reused exactly");
    assert_eq!(s.bump_pointer(), high_water, "no fresh memory should have been taken");
    assert_ne!(replacement.addr(), a.addr());
    s.validate().unwrap();
}

#[test]
fn splitting_a_large_block_leaves_the_remainder_usable() {
    let mut s = FreeListSpace::new(16 << 10);
    let big = s.alloc(0, 512 - HEADER_SIZE).unwrap();
    let _guard = s.alloc(0, 48).unwrap();
    assert_eq!(s.heap().size(big), 512);
    s.free(big);

    let small = s.alloc(0, 48).unwrap();
    assert_eq!(small.addr(), big.addr(), "the small object goes at the front of the hole");
    assert_eq!(s.free_block_count(), 1, "the rest of the hole stays free");
    assert_eq!(s.largest_free_block(), 512 - 64);
    s.validate().unwrap();
}

#[test]
fn the_heap_walk_covers_free_and_live_blocks() {
    let mut s = FreeListSpace::new(16 << 10);
    let hs: Vec<Handle> = (0..40).map(|i| s.alloc(0, 48 + (i % 3) * 16).unwrap()).collect();
    for (i, &h) in hs.iter().enumerate() {
        if i % 3 == 0 {
            s.free(h);
        }
    }
    let walked: u32 = s.walk().map(|h| s.heap().size(h)).sum();
    assert_eq!(walked, s.bump_pointer(), "the walk should cover every byte below the bump");
    assert_eq!(s.live_blocks().len(), 40 - hs.len().div_ceil(3));
    s.validate().unwrap();
}

#[test]
fn exhaustion_is_reported_rather_than_overrunning() {
    let mut s = FreeListSpace::new(Heap::size_for(0, 48) as usize * 4);
    for _ in 0..4 {
        assert!(s.alloc(0, 48).is_some());
    }
    assert!(s.alloc(0, 48).is_none(), "a fifth object does not fit");
    assert_eq!(s.used_bytes(), s.capacity());
    s.validate().unwrap();
}
