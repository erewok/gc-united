//! Module 5 benchmark.
//!
//! Run module 4's benchmark next to this one. Compaction is not a free win, and
//! the numbers say so.
//!
//! What it buys shows up in `fragmentation`, the workload built to defeat
//! non-moving allocators: it stops being a special case, because after a
//! collection there are no holes to fit an object into, only one contiguous run
//! of free space. Allocation is a bump pointer — an add and a bounds check,
//! with no free list to search or split.
//!
//! What it costs shows up everywhere something survives. Module 4 touched the
//! live set once and the heap once; this walks the heap three more times and
//! physically copies every survivor. `binary_trees` keeps a large tree alive
//! for the whole run, and that tree is copied on every single collection, which
//! is why it is *slower* here than under mark-sweep. `deep_chain`, where
//! essentially everything survives, has the longest pause of any module so far.
//!
//! That trade — pay in pause time to buy contiguous memory — is what module 6
//! attacks, by making the cost proportional to what survives rather than to the
//! size of the heap.

use gc_harness::{main_bench, spec};
use gc_mod05::MarkCompact;
use gc_workloads as wl;

const HEAP: usize = 8 << 20;

fn main() {
    main_bench(
        "module 5 — mark-compact",
        MarkCompact::new,
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
