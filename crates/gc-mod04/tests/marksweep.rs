//! What module 4's collector promises.
//!
//! Reachability replaces counting here, so the cycle tests that module 2 could
//! not pass and module 3 needed a whole algorithm for should now pass without
//! the collector doing anything special about cycles at all.

use gc_core::collector::Collector;
use gc_core::mutator::Mutator;
use gc_harness::checks;
use gc_mod04::MarkSweep;
use gc_workloads as wl;

fn mk(capacity: usize) -> MarkSweep {
    MarkSweep::new(capacity)
}

// ---- general conformance --------------------------------------------------

#[test] fn allocates_and_reads_back() { checks::allocates_and_reads_back(mk); }
#[test] fn retains_reachable() { checks::retains_reachable(mk); }
#[test] fn globals_are_roots() { checks::globals_are_roots(mk); }
#[test] fn reclaims_unreachable() { checks::reclaims_unreachable(mk); }
#[test] fn preserves_sharing() { checks::preserves_sharing(mk); }
#[test] fn collects_cycles() { checks::collects_cycles(mk); }
#[test] fn stable_across_repeated_collections() { checks::stable_across_repeated_collections(mk); }
#[test] fn runs_in_a_small_heap() { checks::runs_in_a_small_heap(mk); }
#[test] fn survives_root_churn() { checks::survives_root_churn(mk); }
#[test] fn reports_its_work() { checks::reports_its_work(mk); }
#[test] fn traces_deep_structures() { checks::traces_deep_structures(mk); }

// ---- marking --------------------------------------------------------------

#[test]
fn marking_reaches_everything_live_and_nothing_else() {
    let mut mu = Mutator::new(mk(4 << 20));
    let base = mu.root_depth();

    let live = mu.alloc(1, 8);
    let child = mu.alloc(0, 8);
    mu.store(live, 0, child);
    mu.set_global("live", live);
    let live_h = mu.handle(live);
    let child_h = mu.handle(child);
    mu.unwind(base);

    let garbage = mu.alloc(0, 8);
    let garbage_h = mu.handle(garbage);
    mu.unwind(base);

    mu.gc.mark_from_roots();
    assert!(mu.gc.is_marked(live_h), "an object named by a global is reachable");
    assert!(mu.gc.is_marked(child_h), "an object referenced by a reachable object is reachable");
    assert!(!mu.gc.is_marked(garbage_h), "nothing refers to this object");
}

#[test]
fn objects_reachable_only_through_a_global_are_marked() {
    let mut mu = Mutator::new(mk(1 << 20));
    let base = mu.root_depth();

    let kept = mu.alloc(1, 8);
    let deep = mu.alloc(0, 8);
    mu.store(kept, 0, deep);
    mu.set_global("only_via_global", kept);
    let kept_h = mu.handle(kept);
    let deep_h = mu.handle(deep);
    mu.unwind(base);

    mu.collect();
    assert!(!mu.gc.is_freed(kept_h), "a global is a root and what it names must survive");
    assert!(!mu.gc.is_freed(deep_h), "so must what that object refers to");
    mu.assert_consistent();
}

#[test]
fn marking_a_deep_chain_does_not_use_the_native_stack() {
    let mut mu = Mutator::new(mk(24 << 20));
    let base = mu.root_depth();
    let mut rng = wl::Rng::new(1);

    let head = wl::build_list(&mut mu, 200_000, &mut rng);
    mu.set_global("chain", head);
    mu.unwind(base);

    mu.collect();
    mu.forget_unreachable();
    mu.assert_fully_reclaimed();
}

#[test]
fn a_shared_object_is_scanned_once() {
    let mut mu = Mutator::new(mk(4 << 20));
    let _ = wl::shared_dag(&mut mu, 500, 16, 2);
    mu.collect();

    let marked = mu.stats().objects_marked;
    let reachable = mu.model().reachable().len() as u64;
    assert_eq!(
        marked, reachable,
        "the mark phase visited {marked} objects but only {reachable} are reachable; an \
         object with several referrers is still one object"
    );
    mu.assert_consistent();
}

// ---- sweeping -------------------------------------------------------------

#[test]
fn cycles_need_no_special_handling() {
    let mut mu = Mutator::new(mk(4 << 20));
    let base = mu.root_depth();
    let mut rng = wl::Rng::new(3);

    mu.collect();
    let settled = mu.used_bytes();

    let ring = wl::make_ring(&mut mu, 128, &mut rng);
    let ring_h = mu.handle(ring);
    mu.unwind(base);
    mu.collect();

    assert!(mu.gc.is_freed(ring_h), "the ring is unreachable, so it is garbage");
    assert_eq!(
        mu.used_bytes(),
        settled,
        "all 128 nodes should be gone; {} bytes remain",
        mu.used_bytes() - settled
    );
}

#[test]
fn the_heap_stays_walkable_after_sweeping() {
    let mut mu = Mutator::new(mk(1 << 20));
    let base = mu.root_depth();
    let mut rng = wl::Rng::new(4);

    for round in 0..40 {
        for _ in 0..30 {
            let head = wl::build_list(&mut mu, 12, &mut rng);
            let _ = wl::walk_list(&mut mu, head, 12);
            mu.unwind(base);
        }
        mu.collect();
        mu.gc
            .space_mut()
            .validate()
            .unwrap_or_else(|e| panic!("after collection {round}: {e}"));
    }
}

#[test]
fn a_second_collection_reclaims_what_the_first_kept_alive() {
    let mut mu = Mutator::new(mk(4 << 20));
    let base = mu.root_depth();

    mu.collect();
    let settled = mu.used_bytes();

    let first = wl::make_tree(&mut mu, 8);
    mu.set_global("tree", first);
    mu.unwind(base);
    mu.collect();
    let one_tree = mu.used_bytes();
    assert!(one_tree > settled, "the tree is reachable and must survive");

    // Replace it. The first tree was marked live by the collection above and
    // is garbage now.
    let second = wl::make_tree(&mut mu, 8);
    mu.set_global("tree", second);
    mu.unwind(base);
    mu.collect();

    assert_eq!(
        mu.used_bytes(),
        one_tree,
        "one tree is reachable, exactly as after the first collection, but {} bytes are in \
         use where {one_tree} were",
        mu.used_bytes()
    );
    mu.forget_unreachable();
    mu.assert_fully_reclaimed();
}

#[test]
fn no_mark_bits_survive_a_collection() {
    let mut mu = Mutator::new(mk(4 << 20));
    let base = mu.root_depth();

    let tree = wl::make_tree(&mut mu, 7);
    mu.set_global("tree", tree);
    mu.unwind(base);
    mu.collect();

    let still_marked: Vec<_> = mu
        .gc
        .space()
        .live_blocks()
        .into_iter()
        .filter(|&h| mu.gc.is_marked(h))
        .collect();
    assert!(
        still_marked.is_empty(),
        "{} live objects still carry a mark bit after the collection that set it; the next \
         collection will start from a live set that no longer exists",
        still_marked.len()
    );
}

// ---- triggering -----------------------------------------------------------

#[test]
fn collection_is_triggered_by_allocation() {
    let mut mu = Mutator::new(mk(1 << 20));
    let base = mu.root_depth();
    let mut rng = wl::Rng::new(5);

    for _ in 0..400 {
        let head = wl::build_list(&mut mu, 20, &mut rng);
        let _ = wl::walk_list(&mut mu, head, 20);
        mu.unwind(base);
    }
    assert!(
        mu.stats().collections > 0,
        "{} bytes were allocated into a 1 MiB heap without a collection being triggered",
        mu.stats().bytes_allocated
    );
    assert!(
        mu.stats().objects_freed > 0,
        "collections ran but nothing was reclaimed, though nearly everything allocated is \
         garbage"
    );
}

#[test]
fn a_program_that_keeps_everything_still_finishes() {
    // Nothing here is garbage, so collection can reclaim nothing. The heap
    // should fill up and report that, rather than looping forever collecting.
    let mut mu = Mutator::new(mk(256 << 10));
    let base = mu.root_depth();

    let holder = mu.alloc(64, 0);
    mu.set_global("holder", holder);
    mu.unwind(base);

    let mut chains = 0;
    for i in 0..64u16 {
        let h = mu.global("holder");
        match mu.try_alloc(1, 128) {
            Ok(o) => {
                mu.store(h, i, o);
                chains += 1;
            }
            Err(_) => {
                mu.unwind(base);
                break;
            }
        }
        mu.unwind(base);
    }
    assert!(chains > 0, "at least some objects should fit");
    mu.assert_consistent();
}
