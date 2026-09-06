//! What module 8's collector promises.
//!
//! The hard part of an incremental collector is not marking; it is that the
//! program keeps running while the marking happens. Several of these tests
//! therefore drive a cycle by hand — start it, take a few steps, let the
//! mutator interfere, then finish — because that is the only way to put the
//! collector in the state where the interesting failures happen.

use gc_core::collector::Collector;
use gc_core::heap::{BLACK, WHITE};
use gc_core::mutator::Mutator;
use gc_harness::checks;
use gc_mod08::{Incremental, Phase};
use gc_workloads as wl;

fn mk(capacity: usize) -> Incremental {
    Incremental::new(capacity)
}

/// A chain of `n` nodes, each carrying its index, bound to the global `name`.
fn chain<C: Collector>(mu: &mut Mutator<C>, name: &str, n: u64) {
    let base = mu.root_depth();
    let mut slots = Vec::new();
    for i in 0..n {
        let node = mu.alloc(1, 8);
        mu.write_u64(node, 0, i);
        slots.push(node);
    }
    for i in 0..(n as usize - 1) {
        mu.store(slots[i], 0, slots[i + 1]);
    }
    mu.set_global(name, slots[0]);
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

// ---- the three colours ----------------------------------------------------

#[test]
fn nothing_is_coloured_while_no_cycle_is_running() {
    let mut mu = Mutator::new(mk(1 << 20));
    let base = mu.root_depth();

    let o = mu.alloc(0, 40);
    let h = mu.handle(o);
    assert_eq!(mu.gc.phase(), Phase::Idle, "no cycle has been started");
    assert_eq!(
        mu.gc.color_of(h),
        WHITE,
        "between cycles every object is white; a colour belongs to one cycle"
    );
    assert_eq!(
        mu.gc.grey_count(),
        0,
        "nothing has been reached because nothing is marking"
    );
    mu.unwind(base);
}

#[test]
fn a_finished_cycle_leaves_reachable_objects_black() {
    let mut mu = Mutator::new(mk(1 << 20));
    chain(&mut mu, "chain", 6);

    mu.gc.start_cycle();
    mu.gc.finish_marking();

    let coloured = mu.gc.objects();
    let white: Vec<_> = coloured
        .iter()
        .filter(|&&h| mu.gc.color_of(h) == WHITE)
        .collect();
    assert!(
        white.is_empty(),
        "{} reachable objects were still white when marking finished; marking is over \
         when nothing is grey, and everything reached should be black by then",
        white.len()
    );
    assert!(coloured.iter().all(|&h| mu.gc.color_of(h) == BLACK));
}

#[test]
fn marking_is_done_a_few_objects_at_a_time() {
    let mut mu = Mutator::new(mk(1 << 20));
    chain(&mut mu, "chain", 40);

    mu.gc.start_cycle();
    let mut steps = 0;
    while mu.gc.grey_count() > 0 {
        mu.gc.mark_step(1);
        steps += 1;
        assert!(
            steps < 1000,
            "marking 40 objects has taken {steps} steps and is not finished"
        );
    }
    assert!(
        steps > 1,
        "a chain of 40 objects was marked in {steps} step(s); a collector that marks \
         everything in one step is the stop-the-world collector of module 4"
    );
    assert!(
        mu.stats().increments > 1,
        "increments should count the steps taken"
    );
}

#[test]
fn no_colour_survives_into_the_next_cycle() {
    let mut mu = Mutator::new(mk(1 << 20));
    chain(&mut mu, "chain", 6);

    mu.collect();
    let stragglers: Vec<_> = mu
        .gc
        .objects()
        .into_iter()
        .filter(|&h| mu.gc.color_of(h) != WHITE)
        .collect();
    assert!(
        stragglers.is_empty(),
        "{} objects were still coloured after the sweep; anything left black is treated \
         by the next cycle as already scanned, and whatever it refers to is never reached",
        stragglers.len()
    );
    assert_eq!(mu.gc.phase(), Phase::Idle, "the cycle is over");
    mu.assert_consistent();
}

// ---- the invariant --------------------------------------------------------

#[test]
fn no_black_object_refers_to_a_white_one_when_marking_ends() {
    let mut mu = Mutator::new(mk(1 << 20));
    chain(&mut mu, "chain", 12);
    let base = mu.root_depth();

    mu.gc.start_cycle();
    mu.gc.mark_step(3);

    // Interfere while the wavefront is part way through the chain.
    let head = mu.global("chain");
    let second = mu.load(head, 0);
    mu.store_null(second, 0);
    mu.unwind(base);

    mu.gc.finish_marking();

    let violations: Vec<(u32, u32)> = mu
        .gc
        .objects()
        .into_iter()
        .filter(|&h| mu.gc.color_of(h) == BLACK)
        .flat_map(|h| {
            let kids = mu.gc.heap().children(h);
            kids.into_iter().map(move |c| (h.addr(), c.addr()))
        })
        .filter(|&(_, c)| mu.gc.color_of(gc_core::heap::Handle(c)) == WHITE)
        .collect();

    assert!(
        violations.is_empty(),
        "{} black objects still refer to white ones, starting with {:#x} -> {:#x}; black \
         means the marker is finished with it, so the sweep is about to free something \
         that is still referred to",
        violations.len(),
        violations.first().map(|v| v.0).unwrap_or(0),
        violations.first().map(|v| v.1).unwrap_or(0)
    );
}

#[test]
fn a_reference_moved_behind_the_wavefront_survives() {
    let mut mu = Mutator::new(mk(1 << 20));
    chain(&mut mu, "chain", 8);
    let base = mu.root_depth();

    mu.gc.start_cycle();
    // Blacken the first two nodes; the tail of the chain is still white.
    mu.gc.mark_step(2);

    let head = mu.global("chain");
    let n1 = mu.load(head, 0);
    let n2 = mu.load(n1, 0);
    let n3 = mu.load(n2, 0);
    let n4 = mu.load(n3, 0);
    let n5 = mu.load(n4, 0);
    assert_eq!(
        mu.gc.color_of(mu.handle(head)),
        BLACK,
        "the head should have been scanned"
    );
    assert_eq!(
        mu.gc.color_of(mu.handle(n5)),
        WHITE,
        "the far end of the chain should not have been reached yet"
    );

    // Hang node 5 off the already-scanned head, and cut the path that the
    // marker would have taken to it. Nothing else refers to it now.
    mu.store(head, 0, n5);
    mu.store_null(n4, 0);
    mu.unwind(base);

    mu.gc.finish_marking();
    mu.gc.sweep();

    let head = mu.global("chain");
    let survivor = mu.load(head, 0);
    assert_eq!(
        mu.read_u64(survivor, 0),
        5,
        "node 5 was moved behind the wavefront and the marker had already finished with \
         its new parent, so nothing would ever reach it again"
    );
    mu.unwind(base);
    mu.assert_consistent();
}

#[test]
fn the_barrier_does_nothing_between_cycles() {
    let mut mu = Mutator::new(mk(1 << 20));
    let base = mu.root_depth();

    let parent = mu.alloc(1, 0);
    let child = mu.alloc(0, 40);
    assert_eq!(mu.gc.phase(), Phase::Idle);
    mu.store(parent, 0, child);

    assert_eq!(
        mu.gc.grey_count(),
        0,
        "there is no marking in progress, so there is no wavefront to protect and \
         nothing should have been shaded"
    );
    assert!(
        mu.stats().barrier_hits > 0,
        "the barrier still runs on every store"
    );
    mu.unwind(base);
}

// ---- born mid-cycle, and floating garbage ---------------------------------

#[test]
fn an_object_allocated_during_a_cycle_is_not_swept() {
    let mut mu = Mutator::new(mk(1 << 20));
    // Long enough that one increment cannot finish the cycle, so the
    // allocation below really does happen while marking is in progress.
    chain(&mut mu, "chain", 60);
    let base = mu.root_depth();

    mu.gc.start_cycle();
    let newborn = mu.alloc(0, 40);
    let h = mu.handle(newborn);
    assert_eq!(
        mu.gc.color_of(h),
        BLACK,
        "the roots were shaded before this object existed, so the marker will never \
         reach it; left white, the sweep at the end of this cycle frees it"
    );

    mu.unwind(base);
    mu.gc.finish_marking();
    mu.gc.sweep();

    assert!(
        !mu.gc.is_free(h),
        "an object born during a cycle has to survive that cycle, whether or not \
         anything still refers to it"
    );
}

#[test]
fn an_object_that_dies_after_being_marked_survives_one_cycle() {
    let mut mu = Mutator::new(mk(1 << 20));
    let base = mu.root_depth();

    let doomed = mu.alloc(0, 40);
    mu.set_global("doomed", doomed);
    mu.unwind(base);

    mu.gc.start_cycle();
    mu.gc.finish_marking();

    // It was reachable when the marker looked, and dies immediately after.
    mu.clear_global("doomed");
    let used_before = mu.used_bytes();

    mu.gc.sweep();
    assert_eq!(
        mu.used_bytes(),
        used_before,
        "the object was already black when it died, so this cycle cannot reclaim it; \
         floating garbage is the price of not stopping the world"
    );

    mu.collect();
    assert!(
        mu.used_bytes() < used_before,
        "the next cycle starts with every object white again and should reclaim it, but \
         {} bytes are still in use",
        mu.used_bytes()
    );
}

#[test]
fn a_cycle_driven_entirely_by_allocation_reclaims_garbage() {
    let mut mu = Mutator::new(mk(1 << 20));
    let base = mu.root_depth();
    let mut rng = wl::Rng::new(11);

    for _ in 0..400 {
        let head = wl::build_list(&mut mu, 16, &mut rng);
        let _ = wl::walk_list(&mut mu, head, 16);
        mu.unwind(base);
    }

    assert!(
        mu.stats().increments > 0,
        "marking never advanced; allocation is what is supposed to drive it"
    );
    assert!(
        mu.stats().collections > 0,
        "{} bytes were allocated without a cycle ever completing",
        mu.stats().bytes_allocated
    );
    mu.forget_unreachable();
    mu.assert_consistent();
}
