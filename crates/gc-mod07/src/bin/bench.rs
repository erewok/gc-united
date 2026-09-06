//! Module 7 benchmark.
//!
//! Read the `GCs` and `in gc ms` columns together with module 4's. This
//! collector runs *many* more collections and spends *less* total time
//! collecting, which is the whole generational argument: most collections only
//! look at a small nursery in which almost everything is already dead.
//!
//! `old_to_young` is the workload that pays the write barrier honestly. It
//! keeps a promoted array pointing at freshly allocated objects, so every
//! round creates exactly the edge a minor collection cannot find on its own.
//!
//! `deep_chain` is the unflattering case: a long-lived structure is promoted
//! wholesale, so the nursery work is wasted and the major collections still
//! have to walk all of it.

use gc_harness::{main_bench, spec};
use gc_mod07::Generational;
use gc_workloads as wl;

const HEAP: usize = 16 << 20;

fn main() {
    main_bench(
        "module 7 — generational collection",
        Generational::new,
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
