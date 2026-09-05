//! Module 3 benchmark.
//!
//! Compare this against module 2's numbers. The wall-clock cost of counting is
//! the same, but there is now a collection phase, and the `cyclic` row — which
//! module 2 could not reclaim at all — should end with a live set near zero.
//!
//! Cycle collection only ever examines objects that lost a reference without
//! dying, so its cost tracks the number of suspects rather than the size of the
//! heap. A workload that produces no suspects should show no collection time
//! however much it allocates.

use gc_harness::{main_bench, spec};
use gc_mod03::CycleCollector;
use gc_workloads as wl;

const HEAP: usize = 8 << 20;

fn main() {
    main_bench(
        "module 3 — reference counting with cycle collection",
        CycleCollector::new,
        vec![
            spec("binary_trees", HEAP, |mu| wl::binary_trees(mu, 14)),
            spec("list_churn", HEAP, |mu| wl::list_churn(mu, 2000, 40, 64, 1)),
            spec("shared_dag", HEAP, |mu| wl::shared_dag(mu, 20_000, 32, 2)),
            spec("fragmentation", HEAP, |mu| wl::fragmentation(mu, 40_000, 48, 3)),
            spec("old_to_young", HEAP, |mu| wl::old_to_young(mu, 32, 40_000, 4)),
            spec("cyclic", HEAP, |mu| wl::cyclic_graph(mu, 20_000, 24, 5)),
        ],
    );
}
