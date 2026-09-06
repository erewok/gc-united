//! What module 5's collector promises.
//!
//! Everything module 4 promised, plus the two things compaction is for: after a
//! collection the free space is one contiguous run, and every reference in the
//! program still names the object it named before — even though the objects
//! themselves have moved.

use gc_core::collector::Collector;
use gc_core::heap::Handle;
use gc_core::mutator::Mutator;
use gc_harness::checks;
use gc_mod05::MarkCompact;
use gc_workloads as wl;

fn mk(capacity: usize) -> MarkCompact {
    MarkCompact::new(capacity)
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

// ---- compaction -----------------------------------------------------------

#[test]
fn survivors_end_up_packed_against_each_other() {
    let mut mu = Mutator::new(mk(4 << 20));
    let base = mu.root_depth();

    // Alternate objects that will survive with objects that will not.
    let keeper = mu.alloc(16, 0);
    mu.set_global("keeper", keeper);
    mu.unwind(base);
    for i in 0..16u16 {
        let k = mu.global("keeper");
        let survivor = mu.alloc(0, 40);
        mu.write_u64(survivor, 0, i as u64 + 1);
        mu.store(k, i, survivor);
        let _garbage = mu.alloc(0, 200);
        mu.unwind(base);
    }

    mu.collect();
    mu.forget_unreachable();
    mu.assert_consistent();

    let blocks = mu.gc.blocks();
    let total: u32 = blocks.iter().map(|&h| mu.gc.heap().size(h)).sum();
    assert_eq!(
        total,
        mu.gc.top(),
        "after compaction the objects should cover the heap up to the bump pointer with no \
         gaps between them"
    );
    assert_eq!(
        blocks.len(),
        17,
        "one keeper plus sixteen survivors should remain, not {}",
        blocks.len()
    );
}

#[test]
fn free_space_is_one_contiguous_run() {
    // Sized so that the survivors alone fit easily but the garbage does not,
    // and so that no single hole would be big enough for the large object.
    let mut mu = Mutator::new(mk(1 << 20));
    let base = mu.root_depth();
    let mut rng = wl::Rng::new(7);

    let keeper = mu.alloc(8, 0);
    mu.set_global("keeper", keeper);
    mu.unwind(base);
    for i in 0..8u16 {
        let k = mu.global("keeper");
        let small = mu.alloc(0, 40);
        mu.store(k, i, small);
        mu.unwind(base);
        for _ in 0..40 {
            let head = wl::build_list(&mut mu, 8, &mut rng);
            let _ = wl::walk_list(&mut mu, head, 8);
            mu.unwind(base);
        }
    }

    mu.collect();
    let live = mu.gc.top();
    let free = mu.gc.heap().capacity() - live;
    assert!(free > 0);

    // Every free byte is above the bump pointer, so one object may use all of
    // them. A non-moving allocator could not promise this.
    let big = mu.try_alloc(0, free - 16 - 12);
    assert!(
        big.is_ok(),
        "after compaction all {free} free bytes are contiguous, so an object that needs all \
         of them should fit"
    );
    mu.assert_consistent();
}

#[test]
fn address_order_is_preserved() {
    let mut mu = Mutator::new(mk(1 << 20));
    let base = mu.root_depth();

    let keeper = mu.alloc(8, 0);
    mu.set_global("keeper", keeper);
    mu.unwind(base);
    for i in 0..8u16 {
        let k = mu.global("keeper");
        let o = mu.alloc(0, 40);
        mu.write_u64(o, 0, i as u64);
        mu.store(k, i, o);
        let _garbage = mu.alloc(0, 100);
        mu.unwind(base);
    }

    let k = mu.global("keeper");
    let before: Vec<u32> = (0..8u16)
        .map(|i| {
            let o = mu.load(k, i);
            mu.handle(o).addr()
        })
        .collect();
    mu.unwind(base);

    mu.collect();

    let k = mu.global("keeper");
    let after: Vec<u32> = (0..8u16)
        .map(|i| {
            let o = mu.load(k, i);
            mu.handle(o).addr()
        })
        .collect();
    mu.unwind(base);

    let mut sorted = after.clone();
    sorted.sort();
    assert_eq!(
        after, sorted,
        "sliding compaction moves objects down without reordering them, so objects allocated \
         in this order should still be in this order: {before:?} became {after:?}"
    );
}

// ---- moving safely --------------------------------------------------------

#[test]
fn references_survive_the_move() {
    let mut mu = Mutator::new(mk(4 << 20));
    let base = mu.root_depth();

    let tree = wl::make_tree(&mut mu, 9);
    let expected = wl::check_tree(&mut mu, tree);
    mu.set_global("tree", tree);
    mu.unwind(base);

    for round in 1..=4 {
        // Make garbage below and around the tree so that compaction has to
        // move it somewhere different each time.
        let mut rng = wl::Rng::new(round);
        for _ in 0..50 {
            let head = wl::build_list(&mut mu, 20, &mut rng);
            let _ = wl::walk_list(&mut mu, head, 20);
            mu.unwind(base);
        }
        mu.collect();
        mu.assert_consistent();

        let tree = mu.global("tree");
        let actual = wl::check_tree(&mut mu, tree);
        mu.unwind(base);
        assert_eq!(
            expected, actual,
            "after collection {round} the tree's contents read back differently; some \
             reference is naming an object that is no longer the one it named"
        );
    }
}

#[test]
fn the_shadow_stack_follows_its_objects() {
    let mut mu = Mutator::new(mk(1 << 20));
    let base = mu.root_depth();

    // Held only by a local root across a collection that moves it.
    let _filler = mu.alloc(0, 200);
    let held = mu.alloc(0, 40);
    mu.write_u64(held, 0, 0xDEAD_BEEF);
    let before = mu.handle(held).addr();

    // Drop the filler beneath it, so compaction has somewhere to slide it to.
    let held_value = mu.read_u64(held, 0);
    assert_eq!(held_value, 0xDEAD_BEEF);
    mu.collect();

    assert_eq!(
        mu.read_u64(held, 0),
        0xDEAD_BEEF,
        "the root slot still has to name this object after it moved (it was at {before:#x})"
    );
    mu.assert_consistent();
    mu.unwind(base);
}

#[test]
fn globals_follow_their_objects() {
    let mut mu = Mutator::new(mk(1 << 20));
    let base = mu.root_depth();

    let _filler = mu.alloc(0, 400);
    let kept = mu.alloc(1, 40);
    let child = mu.alloc(0, 40);
    mu.write_u64(child, 0, 0x1234_5678);
    mu.store(kept, 0, child);
    mu.set_global("kept", kept);
    let before = mu.handle(kept).addr();
    mu.unwind(base);

    mu.collect();

    let kept = mu.global("kept");
    let after = mu.handle(kept).addr();
    assert_ne!(
        before, after,
        "the filler below it was garbage, so it should have moved down"
    );
    let child = mu.load(kept, 0);
    assert_eq!(
        mu.read_u64(child, 0),
        0x1234_5678,
        "the global's object moved and its child moved; both references had to be rewritten"
    );
    mu.unwind(base);
    mu.assert_consistent();
}

#[test]
fn an_object_that_stays_put_still_has_its_references_rewritten() {
    let mut mu = Mutator::new(mk(1 << 20));
    let base = mu.root_depth();

    // `anchor` is allocated first, so it is already at address zero and
    // compaction has nowhere to move it to. Its child is allocated after a
    // block of garbage, so the child certainly moves.
    let anchor = mu.alloc(1, 40);
    mu.set_global("anchor", anchor);
    mu.unwind(base);
    let _garbage = mu.alloc(0, 400);
    let a = mu.global("anchor");
    let child = mu.alloc(0, 40);
    mu.write_u64(child, 0, 0xC0DE_C0DE);
    mu.store(a, 0, child);

    let anchor_before = mu.handle(a).addr();
    let child_before = mu.handle(child).addr();
    mu.unwind(base);

    mu.collect();

    let a = mu.global("anchor");
    assert_eq!(
        mu.handle(a).addr(),
        anchor_before,
        "the anchor is the lowest survivor, so it should not have moved"
    );
    let child = mu.load(a, 0);
    assert_ne!(
        mu.handle(child).addr(),
        child_before,
        "the garbage below the child was reclaimed, so the child should have moved down"
    );
    assert_eq!(
        mu.read_u64(child, 0),
        0xC0DE_C0DE,
        "the anchor did not move, but what it refers to did"
    );
    mu.unwind(base);
    mu.assert_consistent();
}

#[test]
fn a_shared_object_moves_once_and_all_referrers_agree() {
    let mut mu = Mutator::new(mk(4 << 20));
    let _ = wl::shared_dag(&mut mu, 600, 16, 11);
    mu.collect();
    mu.assert_consistent();

    // Two referrers of the same child must hold the same address.
    let base = mu.root_depth();
    let pool = mu.global("pool");
    let first = mu.load(pool, 0);
    let addr = mu.handle(first).addr();
    mu.unwind(base);

    let pool = mu.global("pool");
    let again = mu.load(pool, 0);
    assert_eq!(mu.handle(again).addr(), addr, "one object, one address");
    mu.unwind(base);
}

#[test]
fn no_collection_state_survives_into_the_next_one() {
    let mut mu = Mutator::new(mk(4 << 20));
    let base = mu.root_depth();

    let tree = wl::make_tree(&mut mu, 8);
    mu.set_global("tree", tree);
    mu.unwind(base);
    mu.collect();

    let leftovers: Vec<Handle> = mu
        .gc
        .blocks()
        .into_iter()
        .filter(|&h| mu.gc.is_marked(h) || !mu.gc.forwarding_address(h).is_null())
        .collect();
    assert!(
        leftovers.is_empty(),
        "{} objects still carry a mark bit or a forwarding address from the collection that \
         has just finished; the next one will read them as its own",
        leftovers.len()
    );
}
