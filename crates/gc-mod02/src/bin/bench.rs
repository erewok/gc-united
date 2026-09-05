//! Module 2 benchmark.
//!
//! Reference counting has no pauses to measure — the "in gc" column stays near
//! zero because there is no collection phase. The cost is spread through the
//! mutator instead, in every reference assignment, so compare the wall-clock
//! column against the tracing collectors in later modules rather than the gc
//! column.
//!
//! The `cyclic` row is the one to look at. Every object it allocates becomes
//! unreachable, and none of it is reclaimed: watch the live column.

use gc_harness::{main_bench, spec};
use gc_mod02::RefCount;
use gc_workloads as wl;

const HEAP: usize = 8 << 20;

fn main() {
    main_bench(
        "module 2 — reference counting",
        RefCount::new,
        vec![
            spec("binary_trees", HEAP, |mu| wl::binary_trees(mu, 14)),
            spec("list_churn", HEAP, |mu| wl::list_churn(mu, 2000, 40, 64, 1)),
            spec("shared_dag", HEAP, |mu| wl::shared_dag(mu, 20_000, 32, 2)),
            spec("fragmentation", HEAP, |mu| wl::fragmentation(mu, 40_000, 48, 3)),
            spec("old_to_young", HEAP, |mu| wl::old_to_young(mu, 32, 40_000, 4)),
            spec("cyclic", HEAP, |mu| wl::cyclic_graph(mu, 1000, 24, 5)),
        ],
    );
}
