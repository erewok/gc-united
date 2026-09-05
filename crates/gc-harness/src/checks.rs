//! Conformance checks.
//!
//! Each check builds an object graph through the mutator, drives the collector,
//! and then compares the real heap against the independently maintained model.
//! Failures name the object and the path that broke, never the line of code
//! that broke it: locating the defect is the exercise.
//!
//! Every check takes a factory so it can choose its own heap size — several of
//! them depend on the heap being far smaller than the total bytes allocated.

use gc_core::collector::Collector;
use gc_core::mutator::Mutator;
use gc_workloads as wl;

/// 1 MiB, the default for checks with no particular size requirement.
pub const SMALL_HEAP: usize = 1 << 20;
/// 8 MiB.
pub const MEDIUM_HEAP: usize = 8 << 20;
/// 32 MiB, for the deep structure check.
pub const LARGE_HEAP: usize = 32 << 20;

/// Objects can be allocated, written, and read back unchanged.
pub fn allocates_and_reads_back<C: Collector>(mk: impl Fn(usize) -> C) {
    let mut mu = Mutator::new(mk(SMALL_HEAP));
    let base = mu.root_depth();
    for i in 0..64u64 {
        let o = mu.alloc(2, 16);
        mu.write_u64(o, 0, i * 7 + 1);
        mu.write_u64(o, 1, !i);
    }
    for (n, i) in (0..64u64).enumerate() {
        let slot = gc_core::roots::RootSlot(base + n);
        assert_eq!(
            mu.read_u64(slot, 0),
            i * 7 + 1,
            "object {n} came back with a different payload than was written to it"
        );
        assert_eq!(mu.read_u64(slot, 1), !i, "object {n}: second payload word differs");
    }
    mu.assert_consistent();
}

/// Everything reachable from the roots survives a collection.
pub fn retains_reachable<C: Collector>(mk: impl Fn(usize) -> C) {
    let mut mu = Mutator::new(mk(MEDIUM_HEAP));
    let base = mu.root_depth();

    let tree = wl::make_tree(&mut mu, 8);
    let before = wl::check_tree(&mut mu, tree);
    mu.set_global("tree", tree);
    mu.unwind(base);

    for round in 1..=3 {
        mu.collect();
        mu.assert_consistent();
        let tree = mu.global("tree");
        let after = wl::check_tree(&mut mu, tree);
        mu.unwind(base);
        assert_eq!(
            before, after,
            "the live tree's contents changed after collection {round}"
        );
    }
}

/// An object reachable only through a global is a root.
pub fn globals_are_roots<C: Collector>(mk: impl Fn(usize) -> C) {
    let mut mu = Mutator::new(mk(SMALL_HEAP));
    let base = mu.root_depth();

    let kept = mu.alloc(1, 8);
    mu.write_u64(kept, 0, 0xABCD_EF01);
    let child = mu.alloc(0, 8);
    mu.write_u64(child, 0, 0x1234_5678);
    mu.store(kept, 0, child);
    mu.set_global("only_reference", kept);
    mu.unwind(base);

    // Allocate enough garbage to force at least one collection.
    let mut rng = wl::Rng::new(1);
    for _ in 0..200 {
        let head = wl::build_list(&mut mu, 16, &mut rng);
        let _ = wl::walk_list(&mut mu, head, 16);
        mu.unwind(base);
    }
    mu.collect();
    mu.assert_consistent();

    let kept = mu.global("only_reference");
    assert_eq!(mu.read_u64(kept, 0), 0xABCD_EF01, "the global's object was corrupted");
    let child = mu.load(kept, 0);
    assert!(!mu.is_null(child), "the global's object lost its only child");
    assert_eq!(mu.read_u64(child, 0), 0x1234_5678, "the child was corrupted");
    mu.unwind(base);
}

/// Unreachable objects are reclaimed, and nothing else is.
///
/// The live set is replaced wholesale between collections, so each round the
/// objects that were alive during the previous collection are garbage during
/// this one. A collector that only gets the first collection right fails here.
pub fn reclaims_unreachable<C: Collector>(mk: impl Fn(usize) -> C) {
    let mut mu = Mutator::new(mk(MEDIUM_HEAP));
    let base = mu.root_depth();
    let mut rng = wl::Rng::new(7);

    for round in 1..=4 {
        let keep = wl::build_list(&mut mu, 32, &mut rng);
        mu.set_global("keep", keep);
        mu.unwind(base);

        for _ in 0..40 {
            let head = wl::build_list(&mut mu, 32, &mut rng);
            let _ = wl::walk_list(&mut mu, head, 32);
            mu.unwind(base);
        }

        mu.collect();
        mu.forget_unreachable();
        mu.assert_consistent();
        if let Some(v) = gc_core::verify::check_accounting(&mu.gc, mu.model()) {
            panic!("after collection {round}: {v}");
        }
    }
}

/// Shared subgraphs stay shared: one object, many referrers.
pub fn preserves_sharing<C: Collector>(mk: impl Fn(usize) -> C) {
    let mut mu = Mutator::new(mk(MEDIUM_HEAP));
    let _ = wl::shared_dag(&mut mu, 400, 16, 99);
    mu.collect();
    mu.assert_consistent();
    mu.collect();
    mu.assert_consistent();
}

/// Groups of objects that reference each other but nothing else are garbage.
pub fn collects_cycles<C: Collector>(mk: impl Fn(usize) -> C) {
    let mut mu = Mutator::new(mk(MEDIUM_HEAP));
    let base = mu.root_depth();
    let mut rng = wl::Rng::new(11);

    let baseline = {
        mu.collect();
        mu.used_bytes()
    };

    for _ in 0..100 {
        let ring = wl::make_ring(&mut mu, 24, &mut rng);
        let _ = wl::walk_list(&mut mu, ring, 24);
        mu.unwind(base);
    }
    mu.collect();
    mu.forget_unreachable();
    mu.assert_consistent();

    let after = mu.used_bytes();
    assert!(
        after <= baseline,
        "2400 objects were allocated in rings and then abandoned, but {} bytes are \
         still in use where {baseline} were before — objects that refer to each other \
         are not reachable unless something outside the group refers in",
        after
    );
}

/// Tracing a very long chain must not depend on the native call stack.
pub fn traces_deep_structures<C: Collector>(mk: impl Fn(usize) -> C) {
    let mut mu = Mutator::new(mk(LARGE_HEAP));
    let expected = wl::deep_chain(&mut mu, 120_000);
    mu.collect();
    mu.assert_consistent();

    let base = mu.root_depth();
    let head = mu.global("deep_chain");
    let actual = wl::walk_list(&mut mu, head, 120_000);
    mu.unwind(base);
    assert_eq!(expected, actual, "the deep chain's contents changed across collection");
}

/// A live set of constant size must occupy constant space, collection after
/// collection.
///
/// Each round builds a fresh tree of the same shape and drops the previous
/// one, so every collection sees an identically sized live set and a heap full
/// of objects that were alive last time. The footprint should return to the
/// same number every time; if it climbs, something survived that should not
/// have.
pub fn stable_across_repeated_collections<C: Collector>(mk: impl Fn(usize) -> C) {
    let mut mu = Mutator::new(mk(MEDIUM_HEAP));
    let base = mu.root_depth();

    let tree = wl::make_tree(&mut mu, 9);
    mu.set_global("tree", tree);
    mu.unwind(base);
    mu.collect();
    mu.forget_unreachable();
    let settled = mu.used_bytes();

    let mut footprints = vec![settled];
    for round in 2..=8 {
        let tree = wl::make_tree(&mut mu, 9);
        mu.set_global("tree", tree);
        mu.unwind(base);

        mu.collect();
        mu.forget_unreachable();
        mu.assert_consistent();

        let now = mu.used_bytes();
        footprints.push(now);
        assert_eq!(
            settled, now,
            "collection {round} left {now} bytes in use where collection 1 left {settled}, \
             and the live set is the same shape and size every round. Footprint after each \
             collection: {footprints:?}"
        );
    }
}

/// A workload that allocates far more than the heap holds must still finish.
pub fn runs_in_a_small_heap<C: Collector>(mk: impl Fn(usize) -> C) {
    let heap = 256 << 10;
    let mut mu = Mutator::new(mk(heap));
    let base = mu.root_depth();
    let mut rng = wl::Rng::new(23);

    for round in 0..2000 {
        let head = wl::build_list(&mut mu, 24, &mut rng);
        let _ = wl::walk_list(&mut mu, head, 24);
        mu.unwind(base);
        if round % 250 == 0 {
            mu.forget_unreachable();
            mu.assert_consistent();
        }
    }

    let allocated = mu.stats().bytes_allocated;
    assert!(
        allocated > heap as u64 * 4,
        "expected the workload to allocate several times the heap size, but it only \
         allocated {allocated} bytes into a {heap} byte heap"
    );
    mu.collect();
    mu.forget_unreachable();
    mu.assert_fully_reclaimed();
}

/// The collector's own counters must agree with what it actually did.
pub fn reports_its_work<C: Collector>(mk: impl Fn(usize) -> C) {
    let mut mu = Mutator::new(mk(SMALL_HEAP));
    let base = mu.root_depth();
    let mut rng = wl::Rng::new(31);

    for _ in 0..400 {
        let head = wl::build_list(&mut mu, 16, &mut rng);
        let _ = wl::walk_list(&mut mu, head, 16);
        mu.unwind(base);
    }
    mu.collect();
    mu.forget_unreachable();

    let s = mu.stats().clone();
    assert!(s.allocations > 0, "no allocations were counted");
    assert_eq!(
        s.allocations,
        mu.model().len() as u64 + s.objects_freed,
        "allocations counted ({}) should equal the {} objects still reachable plus the {} \
         objects reported freed",
        s.allocations,
        mu.model().len(),
        s.objects_freed
    );
    assert!(
        s.collections > 0,
        "{} bytes were allocated into a {SMALL_HEAP} byte heap without a single collection",
        s.bytes_allocated
    );
    assert!(
        s.bytes_freed > 0,
        "{} collections ran and freed {} objects but reported reclaiming no bytes",
        s.collections,
        s.objects_freed
    );
}

/// Interleaving allocation with root stack churn must not lose objects.
pub fn survives_root_churn<C: Collector>(mk: impl Fn(usize) -> C) {
    let mut mu = Mutator::new(mk(SMALL_HEAP));
    let base = mu.root_depth();
    let mut rng = wl::Rng::new(41);

    let anchor = mu.alloc(4, 8);
    mu.set_global("anchor", anchor);
    mu.unwind(base);

    for round in 0..2000u32 {
        let a = mu.global("anchor");
        let o = mu.alloc(2, 8);
        mu.write_u64(o, 0, round as u64);
        // Half the time the object is retained; the rest becomes garbage as
        // soon as the stack unwinds.
        if rng.next_u64() % 2 == 0 {
            mu.store(a, (round % 4) as u16, o);
        }
        let spare = mu.dup(o);
        mu.assign(spare, a);
        mu.unwind(base);
    }

    mu.collect();
    mu.forget_unreachable();
    mu.assert_fully_reclaimed();
}

/// Every store through the collector reaches its write barrier.
pub fn write_barrier_is_reached<C: Collector>(mk: impl Fn(usize) -> C) {
    let mut mu = Mutator::new(mk(MEDIUM_HEAP));
    let _ = wl::old_to_young(&mut mu, 32, 500, 53);
    assert!(
        mu.stats().barrier_hits > 0,
        "500 reference stores happened without the write barrier being invoked once"
    );
    mu.collect();
    mu.assert_consistent();
}

/// Old objects pointing at young ones keep them alive across a minor
/// collection, which by definition does not trace the old generation.
pub fn old_to_young_edges_survive<C: Collector>(mk: impl Fn(usize) -> C) {
    let mut mu = Mutator::new(mk(MEDIUM_HEAP));
    let expected = wl::old_to_young(&mut mu, 32, 4000, 59);
    mu.assert_consistent();

    let base = mu.root_depth();
    let a = mu.global("old_array");
    let mut actual = 0u64;
    for i in 0..32u16 {
        let o = mu.load(a, i);
        if !mu.is_null(o) {
            actual = actual.wrapping_add(mu.read_u64(o, 0));
        }
        mu.unwind(base + 1);
    }
    mu.unwind(base);
    assert_eq!(
        expected, actual,
        "the old array's young children changed value across minor collections"
    );

    assert!(
        mu.stats().minor_collections > 0,
        "the workload allocated 4000 young objects without a single minor collection"
    );
}

/// Run every general-purpose check that applies to a precise tracing collector.
pub fn tracing_suite<C: Collector>(mk: impl Fn(usize) -> C + Copy) {
    allocates_and_reads_back(mk);
    retains_reachable(mk);
    globals_are_roots(mk);
    reclaims_unreachable(mk);
    preserves_sharing(mk);
    collects_cycles(mk);
    stable_across_repeated_collections(mk);
    runs_in_a_small_heap(mk);
    survives_root_churn(mk);
}
