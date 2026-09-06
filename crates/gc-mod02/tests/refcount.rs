//! What module 2's collector promises.
//!
//! Note what is *not* here: there is no test that abandoned cycles are
//! reclaimed, because reference counting cannot reclaim them.
//! `cycles_are_not_reclaimed` pins down that limitation as a fact about this
//! collector, and module 3 is the answer to it.

use gc_core::collector::Collector;
use gc_core::heap::Handle;
use gc_core::mutator::Mutator;
use gc_harness::checks;
use gc_mod02::RefCount;
use gc_workloads as wl;

fn mk(capacity: usize) -> RefCount {
    RefCount::new(capacity)
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

// ---- counting ------------------------------------------------------------

#[test]
fn a_count_equals_the_number_of_references() {
    let mut mu = Mutator::new(mk(1 << 20));
    let base = mu.root_depth();

    let child = mu.alloc(0, 8);
    let child_h = mu.handle(child);
    assert_eq!(
        mu.gc.rc_of(child_h),
        1,
        "a freshly allocated, rooted object has one reference"
    );

    let a = mu.alloc(2, 8);
    let b = mu.alloc(2, 8);
    mu.store(a, 0, child);
    assert_eq!(mu.gc.rc_of(child_h), 2, "the root plus one field");
    mu.store(b, 0, child);
    assert_eq!(mu.gc.rc_of(child_h), 3, "the root plus two fields");
    mu.store(b, 1, child);
    assert_eq!(mu.gc.rc_of(child_h), 4, "two fields of b both count");

    mu.store_null(b, 1);
    assert_eq!(
        mu.gc.rc_of(child_h),
        3,
        "clearing a field drops a reference"
    );

    mu.unwind(base);
    assert!(
        mu.gc.is_freed(child_h),
        "a, b and the child all went out of scope, so all three should be freed"
    );
}

#[test]
fn storing_a_fields_current_value_back_is_safe() {
    // The assignment `x.field = x.field`, in the case that makes it dangerous:
    // the field holds the only reference to the object. The value does not
    // change, so nothing should be freed, but both a retain and a release
    // happen along the way.
    let mut mu = Mutator::new(mk(1 << 20));
    let base = mu.root_depth();

    let holder = mu.alloc(1, 8);
    let child = mu.alloc(0, 8);
    mu.write_u64(child, 0, 0xFEED_FACE);
    mu.store(holder, 0, child);

    let holder_h = mu.handle(holder);
    let child_h = mu.handle(child);
    mu.unwind(base + 1);
    assert_eq!(
        mu.gc.rc_of(child_h),
        1,
        "the field is now the only reference to the child"
    );

    // Straight through the collector, with no root protecting the value.
    mu.gc.write_field(holder_h, 0, child_h);

    assert!(
        !mu.gc.is_freed(child_h),
        "storing a field's own value back into it freed the object it names"
    );
    assert_eq!(
        mu.gc.rc_of(child_h),
        1,
        "the count should be unchanged: one reference was created and one destroyed"
    );
    assert_eq!(
        mu.gc.heap().data_u64(child_h, 12),
        0xFEED_FACE,
        "the object's payload was overwritten"
    );
    mu.assert_consistent();
}

#[test]
fn overwriting_a_field_releases_what_it_held() {
    let mut mu = Mutator::new(mk(1 << 20));
    let base = mu.root_depth();

    let holder = mu.alloc(1, 8);
    let first = mu.alloc(0, 8);
    let first_h = mu.handle(first);
    mu.store(holder, 0, first);
    mu.unwind(base + 1);
    assert_eq!(mu.gc.rc_of(first_h), 1, "only the field refers to it now");

    let second = mu.alloc(0, 8);
    mu.store(holder, 0, second);
    assert!(
        mu.gc.is_freed(first_h),
        "the field was the last reference to the first object, and it has been overwritten"
    );
}

#[test]
fn rebinding_a_global_releases_what_it_named() {
    let mut mu = Mutator::new(mk(1 << 20));
    let base = mu.root_depth();

    let first = mu.alloc(1, 8);
    let attached = mu.alloc(0, 8);
    mu.store(first, 0, attached);
    let first_h = mu.handle(first);
    let attached_h = mu.handle(attached);
    mu.set_global("current", first);
    mu.unwind(base);
    assert_eq!(mu.gc.rc_of(first_h), 1, "only the global refers to it");

    let second = mu.alloc(0, 8);
    mu.set_global("current", second);
    mu.unwind(base);

    assert!(
        mu.gc.is_freed(first_h),
        "the global was the last reference to the first object, and it now names another"
    );
    assert!(
        mu.gc.is_freed(attached_h),
        "the first object's child lost its last reference along with its parent"
    );
    mu.assert_consistent();
}

// ---- cascading death -----------------------------------------------------

#[test]
fn dropping_a_structure_frees_all_of_it() {
    let mut mu = Mutator::new(mk(4 << 20));
    let base = mu.root_depth();

    let settled = mu.used_bytes();
    let _tree = wl::make_tree(&mut mu, 10);
    let live = mu.used_bytes();
    assert!(live > settled, "the tree should occupy space");

    mu.unwind(base);
    assert_eq!(
        mu.used_bytes(),
        settled,
        "the tree's root lost its last reference, so all {} of its nodes should have been \
         freed with it; {} bytes are still in use",
        (1u32 << 11) - 1,
        mu.used_bytes() - settled
    );
}

#[test]
fn releasing_a_long_chain_does_not_use_the_native_stack() {
    let mut mu = Mutator::new(mk(24 << 20));
    let base = mu.root_depth();
    let mut rng = wl::Rng::new(5);

    let settled = mu.used_bytes();
    let head = wl::build_list(&mut mu, 200_000, &mut rng);
    let _ = head;
    assert!(mu.used_bytes() > settled);

    // Dropping the head kills 200,000 objects in one cascade.
    mu.unwind(base);
    assert_eq!(
        mu.used_bytes(),
        settled,
        "a 200,000 node chain was abandoned; {} bytes survived",
        mu.used_bytes() - settled
    );
}

#[test]
fn a_shared_child_outlives_its_first_parent() {
    let mut mu = Mutator::new(mk(1 << 20));
    let base = mu.root_depth();

    let shared = mu.alloc(0, 8);
    mu.write_u64(shared, 0, 0x5E_A1ED);
    let shared_h = mu.handle(shared);

    let one = mu.alloc(1, 8);
    let two = mu.alloc(1, 8);
    mu.store(one, 0, shared);
    mu.store(two, 0, shared);
    mu.set_global("two", two);
    mu.unwind(base);

    assert!(
        !mu.gc.is_freed(shared_h),
        "the second parent still refers to the shared child"
    );
    assert_eq!(mu.gc.rc_of(shared_h), 1, "exactly one reference remains");
    let _ = one;

    let two = mu.global("two");
    let child = mu.load(two, 0);
    assert_eq!(
        mu.read_u64(child, 0),
        0x5E_A1ED,
        "the shared child was corrupted"
    );
    mu.unwind(base);
    mu.assert_consistent();
}

// ---- the limitation ------------------------------------------------------

#[test]
fn cycles_are_not_reclaimed() {
    // This is not a bug to fix in this module. Counting references cannot see
    // that a group of objects is unreachable when the references keeping them
    // alive come from inside the group. Module 3 adds what is missing.
    let mut mu = Mutator::new(mk(4 << 20));
    let base = mu.root_depth();
    let mut rng = wl::Rng::new(3);

    let before = mu.used_bytes();
    let ring = wl::make_ring(&mut mu, 50, &mut rng);
    let ring_h = mu.handle(ring);
    let occupied = mu.used_bytes() - before;
    assert!(occupied > 0);

    mu.unwind(base);
    mu.collect();

    assert!(
        !mu.gc.is_freed(ring_h),
        "a 50 node ring became unreachable; reference counting alone cannot notice"
    );
    assert_eq!(
        mu.used_bytes() - before,
        occupied,
        "the ring's memory is still held, which is the leak module 3 addresses"
    );
    assert_eq!(
        mu.gc.rc_of(ring_h),
        1,
        "the previous node in the ring still refers to it"
    );
}

#[test]
fn a_self_reference_leaks_too() {
    let mut mu = Mutator::new(mk(1 << 20));
    let base = mu.root_depth();

    let me = mu.alloc(1, 8);
    let me_h: Handle = mu.handle(me);
    mu.store(me, 0, me);
    assert_eq!(
        mu.gc.rc_of(me_h),
        2,
        "one reference from the root, one from itself"
    );

    mu.unwind(base);
    assert_eq!(
        mu.gc.rc_of(me_h),
        1,
        "the object's reference to itself keeps it alive"
    );
    assert!(!mu.gc.is_freed(me_h));
}
