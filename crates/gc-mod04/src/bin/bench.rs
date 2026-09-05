//! Module 4 benchmark.
//!
//! The first collector in the course with real pauses, so the `in gc` and
//! `max pause` columns finally mean something. Two things are worth comparing
//! against modules 2 and 3:
//!
//! The mutator is faster. There is no write barrier and no counting, so storing
//! a reference is a single store instruction again.
//!
//! The pauses are not. Every collection walks the live set and then the entire
//! heap, and the program is stopped for all of it. Watch what happens to
//! `max pause` when the same workload runs in a larger heap: fewer collections,
//! each of them longer.

use gc_harness::{main_bench, spec};
use gc_mod04::MarkSweep;
use gc_workloads as wl;

const HEAP: usize = 8 << 20;

fn main() {
    main_bench(
        "module 4 — mark and sweep",
        MarkSweep::new,
        vec![
            spec("binary_trees", HEAP, |mu| wl::binary_trees(mu, 14)),
            spec("list_churn", HEAP, |mu| wl::list_churn(mu, 2000, 40, 64, 1)),
            spec("shared_dag", HEAP, |mu| wl::shared_dag(mu, 20_000, 32, 2)),
            spec("fragmentation", HEAP, |mu| wl::fragmentation(mu, 40_000, 48, 3)),
            spec("old_to_young", HEAP, |mu| wl::old_to_young(mu, 32, 40_000, 4)),
            spec("cyclic", HEAP, |mu| wl::cyclic_graph(mu, 20_000, 24, 5)),
            spec("deep_chain", 24 << 20, |mu| wl::deep_chain(mu, 150_000)),
        ],
    );
}
