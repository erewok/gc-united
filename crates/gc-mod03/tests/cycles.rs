//! What module 3's collector promises.
//!
//! Everything module 2 guaranteed still has to hold: acyclic garbage must die
//! the moment its last reference goes away, and counts must stay exact. On top
//! of that, abandoned cycles must be found, and — just as important — cycles
//! that are still referenced from outside must be left completely untouched,
//! with their counts restored to exactly what they were.

use gc_core::collector::Collector;
use gc_core::mutator::Mutator;
use gc_harness::checks;
use gc_mod03::{BLACK, CycleCollector, PURPLE};
use gc_workloads as wl;

fn mk(capacity: usize) -> CycleCollector {
    CycleCollector::new(capacity)
}

// ---- general conformance --------------------------------------------------

#[test]
fn allocates_and_reads_back() {
    checks::allocates_and_reads_back(mk);
}
#[test]
fn retains_reachable() {
    checks::retains_reachable(mk);
}
#[test]
fn globals_are_roots() {
    checks::globals_are_roots(mk);
}
#[test]
fn reclaims_unreachable() {
    checks::reclaims_unreachable(mk);
}
#[test]
fn preserves_sharing() {
    checks::preserves_sharing(mk);
}
#[test]
fn collects_cycles() {
    checks::collects_cycles(mk);
}
#[test]
fn stable_across_repeated_collections() {
    checks::stable_across_repeated_collections(mk);
}
#[test]
fn runs_in_a_small_heap() {
    checks::runs_in_a_small_heap(mk);
}
#[test]
fn survives_root_churn() {
    checks::survives_root_churn(mk);
}
#[test]
fn reports_its_work() {
    checks::reports_its_work(mk);
}
#[test]
fn traces_deep_structures() {
    checks::traces_deep_structures(mk);
}

// ---- reference counting still works ---------------------------------------

#[test]
fn acyclic_garbage_still_dies_immediately() {
    let mut mu = Mutator::new(mk(4 << 20));
    let base = mu.root_depth();
    let settled = mu.used_bytes();

    let _tree = wl::make_tree(&mut mu, 9);
    assert!(mu.used_bytes() > settled);
    mu.unwind(base);

    assert_eq!(
        mu.used_bytes(),
        settled,
        "an acyclic tree was abandoned; it should be gone without any collection being run"
    );
    assert_eq!(
        mu.stats().collections,
        0,
        "no collection should have been needed"
    );
}

// ---- finding cycles -------------------------------------------------------

#[test]
fn an_abandoned_ring_is_collected() {
    let mut mu = Mutator::new(mk(4 << 20));
    let base = mu.root_depth();
    let mut rng = wl::Rng::new(1);

    let settled = mu.used_bytes();
    let ring = wl::make_ring(&mut mu, 64, &mut rng);
    let ring_h = mu.handle(ring);
    mu.unwind(base);

    assert!(!mu.gc.is_freed(ring_h), "counting alone cannot free a ring");
    assert_eq!(
        mu.gc.color_of(ring_h),
        PURPLE,
        "the ring's head is a candidate"
    );

    mu.collect();
    assert!(
        mu.gc.is_freed(ring_h),
        "the ring is unreachable and should have been collected"
    );
    assert_eq!(
        mu.used_bytes(),
        settled,
        "all 64 nodes of the ring should be gone; {} bytes remain",
        mu.used_bytes() - settled
    );
    assert_eq!(
        mu.gc.cycles_collected(),
        64,
        "every node of the ring belonged to the cycle"
    );
}

#[test]
fn a_self_reference_is_a_cycle() {
    let mut mu = Mutator::new(mk(1 << 20));
    let base = mu.root_depth();

    let me = mu.alloc(1, 8);
    mu.store(me, 0, me);
    let me_h = mu.handle(me);
    mu.unwind(base);

    assert!(!mu.gc.is_freed(me_h));
    mu.collect();
    assert!(
        mu.gc.is_freed(me_h),
        "an object referring only to itself is garbage"
    );
}

#[test]
fn a_very_long_ring_is_collected() {
    let mut mu = Mutator::new(mk(24 << 20));
    let base = mu.root_depth();
    let mut rng = wl::Rng::new(2);

    let settled = mu.used_bytes();
    let ring = wl::make_ring(&mut mu, 150_000, &mut rng);
    let ring_h = mu.handle(ring);
    mu.unwind(base);
    assert!(!mu.gc.is_freed(ring_h));

    mu.collect();
    assert_eq!(
        mu.used_bytes(),
        settled,
        "a 150,000 node ring should be collected in full; {} bytes remain",
        mu.used_bytes() - settled
    );
}

#[test]
fn two_rings_sharing_a_node_are_both_collected() {
    let mut mu = Mutator::new(mk(4 << 20));
    let base = mu.root_depth();
    let settled = mu.used_bytes();

    // shared -> a -> shared, and shared -> b -> shared, via two slots.
    let shared = mu.alloc(2, 8);
    let a = mu.alloc(2, 8);
    let b = mu.alloc(2, 8);
    mu.store(shared, 0, a);
    mu.store(shared, 1, b);
    mu.store(a, 0, shared);
    mu.store(b, 0, shared);

    let handles = [mu.handle(shared), mu.handle(a), mu.handle(b)];
    mu.unwind(base);
    mu.collect();

    for (name, h) in ["shared", "a", "b"].iter().zip(handles) {
        assert!(
            mu.gc.is_freed(h),
            "{name} is part of an unreachable cycle and should be gone"
        );
    }
    assert_eq!(mu.used_bytes(), settled);
}

// ---- not collecting live cycles -------------------------------------------

#[test]
fn a_ring_referenced_from_outside_survives_untouched() {
    let mut mu = Mutator::new(mk(4 << 20));
    let base = mu.root_depth();
    let mut rng = wl::Rng::new(3);

    let ring = wl::make_ring(&mut mu, 32, &mut rng);
    mu.set_global("ring", ring);
    let ring_h = mu.handle(ring);
    let occupied = mu.used_bytes();

    // Drop the local root so the head becomes a suspect while remaining
    // genuinely reachable, then record every count as it stands going in.
    mu.unwind(base);
    assert_eq!(
        mu.gc.color_of(ring_h),
        PURPLE,
        "the head should be filed as a suspect"
    );
    let counts_before = ring_counts(&mut mu, ring_h, 32);

    mu.collect();

    assert!(
        !mu.gc.is_freed(ring_h),
        "the ring is still named by a global"
    );
    assert_eq!(
        mu.used_bytes(),
        occupied,
        "nothing in the ring was garbage, so nothing should have been freed"
    );
    let counts_after = ring_counts(&mut mu, ring_h, 32);
    assert_eq!(
        counts_before, counts_after,
        "trial deletion took references away from every node in the ring and the collection \
         decided the ring was live; every one of those references has to be put back"
    );
    assert_eq!(
        mu.gc.color_of(ring_h),
        BLACK,
        "a survivor is no longer a suspect"
    );
    mu.assert_consistent();
}

#[test]
fn a_cycle_hanging_off_a_live_object_survives() {
    let mut mu = Mutator::new(mk(4 << 20));
    let base = mu.root_depth();

    let anchor = mu.alloc(1, 8);
    let a = mu.alloc(1, 8);
    let b = mu.alloc(1, 8);
    mu.store(a, 0, b);
    mu.store(b, 0, a);
    mu.store(anchor, 0, a);
    mu.set_global("anchor", anchor);

    let handles = [mu.handle(anchor), mu.handle(a), mu.handle(b)];
    let occupied = mu.used_bytes();
    mu.unwind(base);
    mu.collect();

    for (name, h) in ["anchor", "a", "b"].iter().zip(handles) {
        assert!(
            !mu.gc.is_freed(h),
            "{name} is reachable from a global and must survive"
        );
    }
    assert_eq!(mu.used_bytes(), occupied, "nothing here is garbage");
    mu.assert_consistent();
}

#[test]
fn a_live_cycle_is_collected_once_it_is_abandoned() {
    let mut mu = Mutator::new(mk(4 << 20));
    let base = mu.root_depth();
    let mut rng = wl::Rng::new(4);
    let settled = mu.used_bytes();

    let ring = wl::make_ring(&mut mu, 40, &mut rng);
    mu.set_global("ring", ring);
    let ring_h = mu.handle(ring);
    mu.unwind(base);

    // A collection that finds it live must not stop a later one finding it dead.
    mu.collect();
    assert!(!mu.gc.is_freed(ring_h));

    mu.clear_global("ring");
    mu.collect();
    assert_eq!(
        mu.used_bytes(),
        settled,
        "the ring lost its last external reference and should now be collectable; {} bytes \
         remain",
        mu.used_bytes() - settled
    );
}

#[test]
fn a_suspect_that_is_cleared_can_be_suspected_again() {
    let mut mu = Mutator::new(mk(1 << 20));
    let base = mu.root_depth();
    let settled = mu.used_bytes();

    let obj = mu.alloc(1, 8);
    mu.set_global("obj", obj);
    let obj_h = mu.handle(obj);
    mu.unwind(base);
    assert_eq!(
        mu.gc.color_of(obj_h),
        PURPLE,
        "dropping the local root files it as a suspect"
    );

    // Taking a fresh reference clears the suspicion before the collection runs.
    let held = mu.global("obj");
    assert_eq!(
        mu.gc.color_of(obj_h),
        BLACK,
        "gaining a reference clears the suspicion"
    );
    mu.collect();
    mu.unwind(base);
    let _ = held;

    // Now make it a garbage cycle and abandon it.
    let obj = mu.global("obj");
    mu.store(obj, 0, obj);
    mu.unwind(base);
    mu.clear_global("obj");
    mu.collect();

    assert!(
        mu.gc.is_freed(obj_h),
        "the object was dropped from the candidate buffer by an earlier collection; once it \
         became a garbage cycle it had to be suspected again"
    );
    assert_eq!(mu.used_bytes(), settled);
}

#[test]
fn repeated_cycle_collections_do_not_creep() {
    let mut mu = Mutator::new(mk(4 << 20));
    let base = mu.root_depth();
    let mut rng = wl::Rng::new(5);

    mu.collect();
    let settled = mu.used_bytes();

    for round in 1..=8 {
        for _ in 0..20 {
            let ring = wl::make_ring(&mut mu, 16, &mut rng);
            let _ = wl::walk_list(&mut mu, ring, 16);
            mu.unwind(base);
        }
        mu.collect();
        mu.forget_unreachable();
        mu.assert_consistent();
        assert_eq!(
            mu.used_bytes(),
            settled,
            "round {round} allocated 20 rings and abandoned all of them, but {} bytes are \
             still in use where {settled} were after round 0",
            mu.used_bytes()
        );
    }
}

#[test]
fn only_objects_that_lost_a_reference_become_candidates() {
    let mut mu = Mutator::new(mk(1 << 20));
    let base = mu.root_depth();

    // Allocating and linking never decrements anything, so nothing is suspect.
    let a = mu.alloc(1, 8);
    let b = mu.alloc(1, 8);
    mu.store(a, 0, b);
    assert_eq!(
        mu.gc.candidate_count(),
        0,
        "nothing has lost a reference yet, so there is nothing to suspect"
    );

    mu.unwind(base + 1);
    assert_eq!(
        mu.gc.candidate_count(),
        1,
        "b lost its root but survives through a's field, which makes it a suspect"
    );
}

/// Reference counts of every node in a ring, starting at `head`.
fn ring_counts(
    mu: &mut Mutator<CycleCollector>,
    head: gc_core::heap::Handle,
    len: u32,
) -> Vec<u32> {
    let mut counts = Vec::with_capacity(len as usize);
    let mut cur = head;
    for _ in 0..len {
        counts.push(mu.gc.rc_of(cur));
        cur = mu.gc.heap().field(cur, 0);
    }
    counts
}
