//! Module 6 benchmark.
//!
//! The `alloc MiB` and `in gc ms` columns together make this module's case.
//! `cyclic` allocates roughly twenty megabytes into a four megabyte usable
//! half and spends almost no time collecting, because essentially nothing
//! survives and a copying collector never touches a dead object.
//!
//! `deep_chain` is the opposite and shows the cost honestly: nearly everything
//! survives, so nearly everything is copied, on every collection.
//!
//! Note the heap sizes here are the *total*, and only half of each is usable.
//! That is the trade this collector makes.

use gc_harness::{main_bench, spec};
use gc_mod06::SemiSpace;
use gc_workloads as wl;

const HEAP: usize = 16 << 20;

fn main() {
    main_bench(
        "module 6 — semispace copying",
        SemiSpace::new,
        vec![
            spec("binary_trees", HEAP, |mu| wl::binary_trees(mu, 14)),
            spec("list_churn", HEAP, |mu| wl::list_churn(mu, 2000, 40, 64, 1)),
            spec("shared_dag", HEAP, |mu| wl::shared_dag(mu, 20_000, 32, 2)),
            spec("fragmentation", HEAP, |mu| wl::fragmentation(mu, 40_000, 48, 3)),
            spec("old_to_young", HEAP, |mu| wl::old_to_young(mu, 32, 40_000, 4)),
            spec("cyclic", HEAP, |mu| wl::cyclic_graph(mu, 20_000, 24, 5)),
            spec("deep_chain", 48 << 20, |mu| wl::deep_chain(mu, 150_000)),
        ],
    );
}
