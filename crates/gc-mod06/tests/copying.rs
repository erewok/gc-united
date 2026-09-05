//! What module 6's collector promises.
//!
//! The tests that matter most here are the ones about *identity*. A copying
//! collector that duplicates a shared object produces a heap that passes every
//! casual inspection: the objects are all there, the fields all point at
//! well-formed objects, the payloads are all intact. It is simply not the
//! program's object graph any more.

use gc_core::collector::Collector;
use gc_core::mutator::Mutator;
use gc_harness::checks;
use gc_mod06::SemiSpace;
use gc_workloads as wl;

fn mk(capacity: usize) -> SemiSpace {
    SemiSpace::new(capacity)
}

// A semispace collector can only use half of what it is given.
fn mk2(capacity: usize) -> SemiSpace {
    SemiSpace::new(capacity * 2)
}

// ---- general conformance --------------------------------------------------

#[test] fn allocates_and_reads_back() { checks::allocates_and_reads_back(mk2); }
#[test] fn retains_reachable() { checks::retains_reachable(mk2); }
#[test] fn globals_are_roots() { checks::globals_are_roots(mk2); }
#[test] fn reclaims_unreachable() { checks::reclaims_unreachable(mk2); }
#[test] fn preserves_sharing() { checks::preserves_sharing(mk2); }
#[test] fn collects_cycles() { checks::collects_cycles(mk2); }
#[test] fn stable_across_repeated_collections() { checks::stable_across_repeated_collections(mk2); }
#[test] fn runs_in_a_small_heap() { checks::runs_in_a_small_heap(mk2); }
#[test] fn survives_root_churn() { checks::survives_root_churn(mk2); }
#[test] fn reports_its_work() { checks::reports_its_work(mk2); }
#[test] fn traces_deep_structures() { checks::traces_deep_structures(mk2); }

// ---- the two halves -------------------------------------------------------

#[test]
fn collection_swaps_the_halves() {
    let mut mu = Mutator::new(mk(1 << 20));
    let base = mu.root_depth();

    let kept = mu.alloc(0, 40);
    mu.write_u64(kept, 0, 0xA11FE);
    mu.set_global("kept", kept);
    mu.unwind(base);

    let first_space = mu.gc.current_space();
    assert!(mu.gc.in_current_space(mu.gc.roots().global("kept")));

    mu.collect();
    let second_space = mu.gc.current_space();
    assert_ne!(
        first_space, second_space,
        "a collection copies the survivors into the other half and allocates there afterwards"
    );

    let kept = mu.global("kept");
    assert_eq!(mu.read_u64(kept, 0), 0xA11FE, "the survivor's payload should have come with it");
    assert!(
        mu.gc.in_current_space(mu.handle(kept)),
        "the survivor should now live in the half being allocated into"
    );
    mu.unwind(base);

    mu.collect();
    assert_eq!(mu.gc.current_space(), first_space, "a second collection swaps back");
}

#[test]
fn only_half_the_heap_is_usable() {
    let mut mu = Mutator::new(mk(1 << 20));
    assert_eq!(mu.gc.half_size() * 2, mu.gc.heap().capacity());

    // Fill the current half with objects that all stay alive.
    let base = mu.root_depth();
    let mut n = 0;
    while mu.try_alloc(0, 40).is_ok() {
        n += 1;
        if n > 200_000 {
            panic!("allocation never failed; the collector is using more than one half");
        }
    }
    assert!(
        mu.gc.free_pointer() <= mu.gc.limit(),
        "allocation ran past the end of its half, into the space reserved for copying"
    );
    mu.unwind(base);
}

#[test]
fn survivors_are_packed_with_no_gaps() {
    let mut mu = Mutator::new(mk(4 << 20));
    let base = mu.root_depth();

    let keeper = mu.alloc(16, 0);
    mu.set_global("keeper", keeper);
    mu.unwind(base);
    for i in 0..16u16 {
        let k = mu.global("keeper");
        let survivor = mu.alloc(0, 40);
        mu.store(k, i, survivor);
        let _garbage = mu.alloc(0, 300);
        mu.unwind(base);
    }

    mu.collect();
    let objects = mu.gc.objects();
    let total: u32 = objects.iter().map(|&h| mu.gc.heap().size(h)).sum();
    assert_eq!(
        total,
        mu.gc.used_bytes(),
        "copying into a fresh space packs the survivors together, so there should be no gaps"
    );
    assert_eq!(objects.len(), 17, "a keeper and sixteen survivors");
}

// ---- identity -------------------------------------------------------------

#[test]
fn a_shared_object_is_copied_once() {
    let mut mu = Mutator::new(mk(1 << 20));
    let base = mu.root_depth();

    let shared = mu.alloc(0, 40);
    mu.write_u64(shared, 0, 0x5EA1_ED);
    let one = mu.alloc(1, 8);
    let two = mu.alloc(1, 8);
    let three = mu.alloc(1, 8);
    for parent in [one, two, three] {
        mu.store(parent, 0, shared);
    }
    let holder = mu.alloc(3, 0);
    mu.store(holder, 0, one);
    mu.store(holder, 1, two);
    mu.store(holder, 2, three);
    mu.set_global("holder", holder);
    mu.unwind(base);

    let copied_before = mu.stats().objects_copied;
    mu.collect();
    let copied = mu.stats().objects_copied - copied_before;
    assert_eq!(
        copied, 5,
        "five objects are reachable — a holder, three parents and the one object all three \
         of them share — but {copied} copies were made"
    );

    // All three parents must name the same address.
    let h = mu.global("holder");
    let mut addrs = Vec::new();
    for i in 0..3u16 {
        let parent = mu.load(h, i);
        let child = mu.load(parent, 0);
        addrs.push(mu.handle(child).addr());
        mu.unwind(base + 1);
    }
    mu.unwind(base);
    assert_eq!(
        addrs[0], addrs[1],
        "the first two parents referred to one object and now refer to {:#x} and {:#x}",
        addrs[0], addrs[1]
    );
    assert_eq!(addrs[1], addrs[2], "and so does the third");
    mu.assert_consistent();
}

#[test]
fn writing_through_one_reference_is_seen_through_another() {
    let mut mu = Mutator::new(mk(1 << 20));
    let base = mu.root_depth();

    let shared = mu.alloc(0, 40);
    let left = mu.alloc(1, 8);
    let right = mu.alloc(1, 8);
    mu.store(left, 0, shared);
    mu.store(right, 0, shared);
    let holder = mu.alloc(2, 0);
    mu.store(holder, 0, left);
    mu.store(holder, 1, right);
    mu.set_global("holder", holder);
    mu.unwind(base);

    mu.collect();

    let h = mu.global("holder");
    let l = mu.load(h, 0);
    let via_left = mu.load(l, 0);
    mu.write_u64(via_left, 0, 0xFACE_FEED);
    mu.unwind(base + 1);

    let h = mu.global("holder");
    let r = mu.load(h, 1);
    let via_right = mu.load(r, 0);
    assert_eq!(
        mu.read_u64(via_right, 0),
        0xFACE_FEED,
        "both parents referred to the same object before the collection, so a write through \
         one must be visible through the other"
    );
    mu.unwind(base);
}

#[test]
fn a_cycle_is_copied_without_looping_forever() {
    let mut mu = Mutator::new(mk(4 << 20));
    let base = mu.root_depth();
    let mut rng = wl::Rng::new(2);

    let ring = wl::make_ring(&mut mu, 64, &mut rng);
    let expected = wl::walk_list(&mut mu, ring, 64);
    mu.set_global("ring", ring);
    mu.unwind(base);

    let copied_before = mu.stats().objects_copied;
    mu.collect();
    assert_eq!(
        mu.stats().objects_copied - copied_before,
        64,
        "a ring of 64 objects is 64 objects, however many times the traversal goes round it"
    );

    let ring = mu.global("ring");
    let actual = wl::walk_list(&mut mu, ring, 64);
    mu.unwind(base);
    assert_eq!(expected, actual, "the ring's contents changed");
    mu.assert_consistent();
}

// ---- the scan loop --------------------------------------------------------

#[test]
fn objects_copied_late_are_scanned_too() {
    // A deliberately long, thin graph: each object is only discovered by
    // scanning the one before it, so the last objects reached are copied after
    // scanning has already begun.
    let mut mu = Mutator::new(mk(8 << 20));
    let base = mu.root_depth();
    let mut rng = wl::Rng::new(3);

    let head = wl::build_list(&mut mu, 20_000, &mut rng);
    let expected = wl::walk_list(&mut mu, head, 20_000);
    mu.set_global("chain", head);
    mu.unwind(base);

    mu.collect();
    mu.assert_consistent();

    let head = mu.global("chain");
    let actual = wl::walk_list(&mut mu, head, 20_000);
    mu.unwind(base);
    assert_eq!(
        expected, actual,
        "every object in the chain has to be copied and have its own reference updated, \
         including the ones discovered last"
    );
}

#[test]
fn nothing_still_points_into_the_abandoned_half() {
    let mut mu = Mutator::new(mk(4 << 20));
    let base = mu.root_depth();

    let tree = wl::make_tree(&mut mu, 9);
    mu.set_global("tree", tree);
    mu.unwind(base);
    mu.collect();

    let strays: Vec<_> = mu
        .gc
        .objects()
        .into_iter()
        .flat_map(|h| {
            let children = mu.gc.heap().children(h);
            children.into_iter().map(move |c| (h, c))
        })
        .filter(|&(_, c)| !mu.gc.in_current_space(c))
        .collect();
    assert!(
        strays.is_empty(),
        "{} references still point into the half that was abandoned, starting with {:?}",
        strays.len(),
        strays.first()
    );
    mu.assert_consistent();
}

#[test]
fn a_collection_that_saves_nothing_costs_nothing() {
    let mut mu = Mutator::new(mk(1 << 20));
    let base = mu.root_depth();
    let mut rng = wl::Rng::new(4);

    // Build and abandon; nothing is reachable when the collection runs.
    for _ in 0..20 {
        let head = wl::build_list(&mut mu, 30, &mut rng);
        let _ = wl::walk_list(&mut mu, head, 30);
        mu.unwind(base);
    }
    let copied_before = mu.stats().objects_copied;
    mu.collect();

    assert_eq!(
        mu.stats().objects_copied - copied_before,
        0,
        "nothing was reachable, so nothing should have been copied"
    );
    assert_eq!(mu.used_bytes(), 0, "the new half should be completely empty");
    assert!(mu.gc.empty_collections() > 0);
}
