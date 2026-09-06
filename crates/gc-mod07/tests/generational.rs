//! What module 7's collector promises.
//!
//! Most of these tests are about the seam between the two generations. A
//! generational collector that traces the whole heap on every collection is
//! *correct* and pointless; one that skips the old generation without a working
//! write barrier is fast and silently loses objects. The interesting failures
//! all live in between.

use gc_core::collector::Collector;
use gc_core::mutator::Mutator;
use gc_harness::checks;
use gc_mod07::{Generational, OLD, YOUNG};
use gc_workloads as wl;

fn mk(capacity: usize) -> Generational {
    Generational::new(capacity)
}

/// Allocate an object, bind it to `name`, and promote it out of the nursery.
fn make_old<C: Collector>(mu: &mut Mutator<C>, name: &str, nrefs: u16) {
    let base = mu.root_depth();
    let o = mu.alloc(nrefs, 0);
    mu.set_global(name, o);
    mu.unwind(base);
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
#[test]
fn write_barrier_is_reached() {
    checks::write_barrier_is_reached(mk);
}
#[test]
fn old_to_young_edges_survive() {
    checks::old_to_young_edges_survive(mk);
}

// ---- the two generations --------------------------------------------------

#[test]
fn new_objects_are_born_in_the_nursery() {
    let mut mu = Mutator::new(mk(4 << 20));
    let base = mu.root_depth();

    let o = mu.alloc(0, 40);
    let h = mu.handle(o);
    assert!(
        mu.gc.is_young(h),
        "a freshly allocated object belongs in the nursery"
    );
    assert_eq!(
        mu.gc.heap().generation(h),
        YOUNG,
        "and should say so in its header"
    );
    mu.unwind(base);
}

#[test]
fn surviving_a_minor_collection_promotes_into_the_old_generation() {
    let mut mu = Mutator::new(mk(4 << 20));
    let base = mu.root_depth();

    let kept = mu.alloc(0, 40);
    mu.write_u64(kept, 0, 0xC0FFEE);
    mu.set_global("kept", kept);
    mu.unwind(base);
    assert!(mu.gc.is_young(mu.gc.roots().global("kept")));

    mu.gc.minor_collect();

    let h = mu.gc.roots().global("kept");
    assert!(
        !mu.gc.is_young(h),
        "an object still reachable when the nursery is collected has to leave it"
    );
    assert_eq!(
        mu.gc.heap().generation(h),
        OLD,
        "and should be recorded as old"
    );

    let k = mu.global("kept");
    assert_eq!(
        mu.read_u64(k, 0),
        0xC0FFEE,
        "the survivor's payload should have moved with it"
    );
    mu.unwind(base);
    mu.assert_consistent();
}

#[test]
fn a_minor_collection_leaves_the_nursery_empty() {
    let mut mu = Mutator::new(mk(4 << 20));
    let base = mu.root_depth();

    for _ in 0..10 {
        let _garbage = mu.alloc(0, 40);
    }
    mu.unwind(base);
    assert!(
        mu.gc.young_used() > 0,
        "the nursery should hold what was just allocated"
    );

    mu.gc.minor_collect();
    assert_eq!(
        mu.gc.young_used(),
        0,
        "the whole nursery is reclaimed at once, whatever was in it"
    );
}

#[test]
fn a_shared_young_object_is_promoted_once() {
    let mut mu = Mutator::new(mk(4 << 20));
    let base = mu.root_depth();

    let shared = mu.alloc(0, 40);
    let one = mu.alloc(1, 8);
    let two = mu.alloc(1, 8);
    mu.store(one, 0, shared);
    mu.store(two, 0, shared);
    let holder = mu.alloc(2, 0);
    mu.store(holder, 0, one);
    mu.store(holder, 1, two);
    mu.set_global("holder", holder);
    mu.unwind(base);

    let before = mu.stats().objects_promoted;
    mu.gc.minor_collect();
    let promoted = mu.stats().objects_promoted - before;
    assert_eq!(
        promoted, 4,
        "a holder, two parents and the one object both of them share is four objects, \
         but {promoted} were promoted"
    );

    let h = mu.global("holder");
    let l = mu.load(h, 0);
    let via_left = mu.load(l, 0);
    let left_addr = mu.handle(via_left).addr();
    mu.unwind(base);

    let h = mu.global("holder");
    let r = mu.load(h, 1);
    let via_right = mu.load(r, 0);
    let right_addr = mu.handle(via_right).addr();
    mu.unwind(base);

    assert_eq!(
        left_addr, right_addr,
        "both parents referred to one object before promotion and now name {left_addr:#x} \
         and {right_addr:#x}"
    );
    mu.assert_consistent();
}

// ---- the write barrier ----------------------------------------------------

#[test]
fn an_old_object_pointing_at_a_young_one_keeps_it_alive() {
    let mut mu = Mutator::new(mk(4 << 20));
    let base = mu.root_depth();

    make_old(&mut mu, "old", 2);
    mu.gc.minor_collect();
    let a = mu.global("old");
    assert!(
        !mu.gc.is_young(mu.handle(a)),
        "the array should have been promoted"
    );

    let young = mu.alloc(0, 40);
    mu.write_u64(young, 0, 0xBEEF);
    mu.store(a, 0, young);
    // The young object is now reachable only through the old array.
    mu.unwind(base);

    mu.gc.minor_collect();

    let a = mu.global("old");
    let child = mu.load(a, 0);
    assert!(
        !mu.is_null(child),
        "the old array's only child was reclaimed; a minor collection never traces the \
         old generation, so nothing found that reference"
    );
    assert_eq!(
        mu.read_u64(child, 0),
        0xBEEF,
        "the surviving child holds the wrong payload"
    );
    mu.unwind(base);
    mu.assert_consistent();
}

#[test]
fn the_write_barrier_records_old_to_young_edges() {
    let mut mu = Mutator::new(mk(4 << 20));
    let base = mu.root_depth();

    make_old(&mut mu, "old", 2);
    mu.gc.minor_collect();
    assert_eq!(mu.gc.remembered_count(), 0, "nothing has been stored yet");

    let a = mu.global("old");
    let young = mu.alloc(0, 40);
    mu.store(a, 0, young);
    mu.unwind(base);

    assert_eq!(
        mu.gc.remembered_count(),
        1,
        "an old object was pointed at a young one and nothing recorded it"
    );
}

#[test]
fn only_old_to_young_edges_are_remembered() {
    let mut mu = Mutator::new(mk(4 << 20));
    let base = mu.root_depth();

    // Young to young: a minor collection traces the whole nursery, so this
    // edge is found without anyone recording it.
    let parent = mu.alloc(1, 0);
    let child = mu.alloc(0, 40);
    mu.store(parent, 0, child);
    assert_eq!(
        mu.gc.remembered_count(),
        0,
        "a young-to-young reference is discovered by tracing the nursery; remembering it \
         only gives the next minor collection more to scan"
    );
    mu.unwind(base);

    // Young to old: the target is not in the nursery, so this edge can never
    // be the one a minor collection is missing.
    make_old(&mut mu, "old", 0);
    mu.gc.minor_collect();
    let holder = mu.alloc(1, 0);
    let o = mu.global("old");
    mu.store(holder, 0, o);
    assert_eq!(
        mu.gc.remembered_count(),
        0,
        "a reference that does not point into the nursery is not an old-to-young edge"
    );
    mu.unwind(base);
}

#[test]
fn an_edge_overwritten_many_times_is_remembered_once() {
    let mut mu = Mutator::new(mk(4 << 20));
    let base = mu.root_depth();

    make_old(&mut mu, "old", 2);
    mu.gc.minor_collect();

    for _ in 0..50 {
        let a = mu.global("old");
        let young = mu.alloc(0, 40);
        mu.store(a, 0, young);
        mu.unwind(base);
    }

    assert_eq!(
        mu.gc.remembered_count(),
        1,
        "one old object was pointed at fifty different young ones; it is still one old \
         object, and a remembered set that grows every time is unbounded"
    );
}

#[test]
fn a_minor_collection_clears_the_remembered_set() {
    let mut mu = Mutator::new(mk(4 << 20));
    let base = mu.root_depth();

    make_old(&mut mu, "old", 2);
    mu.gc.minor_collect();

    let a = mu.global("old");
    let young = mu.alloc(0, 40);
    mu.store(a, 0, young);
    mu.unwind(base);
    assert!(mu.gc.remembered_count() > 0);

    mu.gc.minor_collect();
    assert_eq!(
        mu.gc.remembered_count(),
        0,
        "the nursery is empty afterwards, so no old object can still point into it, and \
         entries carried over would be scanned again forever"
    );
    mu.assert_consistent();
}

// ---- what a minor collection costs ---------------------------------------

#[test]
fn a_minor_collection_does_not_trace_the_old_generation() {
    let mut mu = Mutator::new(mk(4 << 20));
    let base = mu.root_depth();

    // Build a substantial old structure.
    let head = wl::build_list(&mut mu, 200, &mut wl::Rng::new(7));
    mu.set_global("old_list", head);
    mu.unwind(base);
    mu.gc.minor_collect();

    let marked_before = mu.stats().objects_marked;
    for _ in 0..10 {
        let _garbage = mu.alloc(0, 40);
    }
    mu.unwind(base);
    mu.gc.minor_collect();

    let marked = mu.stats().objects_marked - marked_before;
    assert_eq!(
        marked, 0,
        "a minor collection marked {marked} objects; it is supposed to look at the nursery \
         and nothing else, which is the only reason it is cheap"
    );
}

#[test]
fn dead_young_objects_are_counted_as_freed() {
    let mut mu = Mutator::new(mk(4 << 20));
    let base = mu.root_depth();

    let before = mu.stats().objects_freed;
    for _ in 0..100 {
        let _garbage = mu.alloc(0, 40);
    }
    mu.unwind(base);
    mu.gc.minor_collect();

    let freed = mu.stats().objects_freed - before;
    assert_eq!(
        freed, 100,
        "100 objects died in the nursery and {freed} were reported freed; they are \
         reclaimed without being examined, but they still have to be counted"
    );
}

#[test]
fn a_major_collection_reclaims_old_garbage_that_a_minor_cannot() {
    let mut mu = Mutator::new(mk(4 << 20));
    let base = mu.root_depth();

    for i in 0..20 {
        let o = mu.alloc(0, 40);
        mu.set_global(&format!("g{i}"), o);
        mu.unwind(base);
    }
    mu.gc.minor_collect();
    let promoted_bytes = mu.gc.old_used();
    assert!(
        promoted_bytes > 0,
        "the twenty globals should have been promoted"
    );

    for i in 0..20 {
        mu.clear_global(&format!("g{i}"));
    }

    mu.gc.minor_collect();
    assert_eq!(
        mu.gc.old_used(),
        promoted_bytes,
        "a minor collection does not examine the old generation, so abandoned old objects \
         must still be occupying it"
    );

    mu.gc.major_collect();
    assert!(
        mu.gc.old_used() < promoted_bytes,
        "a major collection should have reclaimed the abandoned old objects, but the old \
         generation still holds {} bytes",
        mu.gc.old_used()
    );
    mu.assert_consistent();
}
