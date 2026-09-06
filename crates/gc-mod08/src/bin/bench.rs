//! Module 8 benchmark.
//!
//! The column to read here is **max pause**, against module 4's. Both are
//! mark-sweep collectors and their totals are broadly comparable; the
//! difference is that module 4 does its marking in one stop and this one
//! spreads the same work across thousands of allocations.
//!
//! `in gc ms` will not be lower — incremental marking does *more* total work,
//! because the barrier runs on every store and floating garbage means some
//! objects are marked in a cycle that could have skipped them. That is the
//! trade: throughput for responsiveness.

use gc_harness::{main_bench, spec};
use gc_mod08::Incremental;
use gc_workloads as wl;

const HEAP: usize = 16 << 20;

fn main() {
    main_bench(
        "module 8 — incremental tri-colour marking",
        Incremental::new,
        vec![
            spec("binary_trees", HEAP, |mu| wl::binary_trees(mu, 14)),
            spec("list_churn", HEAP, |mu| wl::list_churn(mu, 2000, 40, 64, 1)),
            spec("shared_dag", HEAP, |mu| wl::shared_dag(mu, 20_000, 32, 2)),
            spec("fragmentation", HEAP, |mu| {
                wl::fragmentation(mu, 40_000, 48, 3)
            }),
            spec("old_to_young", HEAP, |mu| {
                wl::old_to_young(mu, 32, 40_000, 4)
            }),
            spec("cyclic", HEAP, |mu| wl::cyclic_graph(mu, 20_000, 24, 5)),
            spec("deep_chain", 48 << 20, |mu| wl::deep_chain(mu, 150_000)),
        ],
    );
}
