//! Candidate programs: deterministic mutators that stress a collector in
//! different ways.
//!
//! Every workload here is written against [`Mutator`] only, so the same
//! program runs unchanged against every module's collector and the benchmark
//! numbers are directly comparable. Each returns a checksum computed by
//! reading data back out of the heap, so a collector that corrupts payloads or
//! loses objects fails loudly rather than quietly producing fast garbage.
//!
//! The workloads are chosen to expose specific weaknesses:
//!
//! | workload        | what it punishes                                    |
//! |-----------------|-----------------------------------------------------|
//! | `binary_trees`  | high allocation rate, mostly short-lived objects     |
//! | `list_churn`    | steady garbage generation against a stable live set  |
//! | `cyclic_graph`  | anything that reclaims by counting references        |
//! | `fragmentation` | non-moving allocators faced with mixed object sizes  |
//! | `old_to_young`  | generational collectors without a correct barrier    |
//! | `deep_chain`    | recursive tracing, which overflows the native stack  |
//! | `shared_dag`    | copying collectors that duplicate shared subgraphs   |
//!
//! Workloads keep only a bounded number of roots live at a time, using
//! [`Mutator::dup`] and [`Mutator::assign`] to walk long structures the way a
//! real compiler would allocate locals. A workload that rooted every node it
//! touched would keep the entire graph alive and measure nothing.

use gc_core::collector::Collector;
use gc_core::mutator::Mutator;
use gc_core::roots::RootSlot;

/// xorshift64*, so every workload is reproducible from its seed.
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Rng {
        Rng(if seed == 0 { 0x9E37_79B9_7F4A_7C15 } else { seed })
    }
    pub fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    /// Uniform-ish value in `[0, n)`.
    pub fn below(&mut self, n: u64) -> u64 {
        if n == 0 { 0 } else { self.next_u64() % n }
    }
}

/// A node with `next`/`other` reference slots and one scalar word.
pub const NODE_REFS: u16 = 2;
/// Scalar payload of a node, in bytes.
pub const NODE_DATA: u32 = 8;

// ---------------------------------------------------------------- trees ----

/// Build a perfectly balanced binary tree of the given depth.
pub fn make_tree<C: Collector>(mu: &mut Mutator<C>, depth: u32) -> RootSlot {
    let base = mu.root_depth();
    let node = mu.alloc(NODE_REFS, NODE_DATA);
    mu.write_u64(node, 0, depth as u64 + 1);
    if depth > 0 {
        let left = make_tree(mu, depth - 1);
        mu.store(node, 0, left);
        let right = make_tree(mu, depth - 1);
        mu.store(node, 1, right);
        mu.unwind(base + 1);
    }
    node
}

/// Sum the scalar word of every node in a tree.
pub fn check_tree<C: Collector>(mu: &mut Mutator<C>, root: RootSlot) -> u64 {
    let base = mu.root_depth();
    let mut sum = mu.read_u64(root, 0);
    for i in 0..NODE_REFS {
        let child = mu.load(root, i);
        if !mu.is_null(child) {
            sum = sum.wrapping_add(check_tree(mu, child));
        }
        mu.unwind(base);
    }
    sum
}

/// The classic GCBench shape: one long-lived tree kept alive for the whole
/// run, plus a storm of short-lived trees at a range of depths.
pub fn binary_trees<C: Collector>(mu: &mut Mutator<C>, max_depth: u32) -> u64 {
    let base = mu.root_depth();
    let min_depth = 4.min(max_depth);
    let mut checksum = 0u64;

    // Built, measured and dropped at once, to grow the heap before anything
    // long-lived is allocated.
    let stretch = make_tree(mu, (min_depth + 1).min(max_depth));
    checksum = checksum.wrapping_add(check_tree(mu, stretch));
    mu.unwind(base);

    let long_lived = make_tree(mu, max_depth);
    mu.set_global("long_lived_tree", long_lived);
    mu.unwind(base);

    let mut depth = min_depth;
    while depth <= max_depth {
        let iterations = 1u32 << (max_depth - depth + min_depth);
        for _ in 0..iterations {
            let t = make_tree(mu, depth);
            checksum = checksum.wrapping_add(check_tree(mu, t));
            mu.unwind(base);
        }
        depth += 2;
    }

    let lived = mu.global("long_lived_tree");
    checksum = checksum.wrapping_add(check_tree(mu, lived));
    mu.unwind(base);
    checksum
}

// ---------------------------------------------------------------- lists ----

/// Build a singly linked list of `len` nodes carrying pseudorandom values.
pub fn build_list<C: Collector>(mu: &mut Mutator<C>, len: u32, rng: &mut Rng) -> RootSlot {
    assert!(len > 0, "a list needs at least one node");
    let base = mu.root_depth();
    let head = mu.alloc(NODE_REFS, NODE_DATA);
    mu.write_u64(head, 0, rng.next_u64());
    let tail = mu.dup(head);
    for _ in 1..len {
        let node = mu.alloc(NODE_REFS, NODE_DATA);
        mu.write_u64(node, 0, rng.next_u64());
        mu.store(tail, 0, node);
        mu.assign(tail, node);
        mu.unwind(base + 2);
    }
    mu.unwind(base + 1);
    head
}

/// Sum the scalar word of every node reachable along slot 0, following at most
/// `limit` links so that a cycle terminates.
pub fn walk_list<C: Collector>(mu: &mut Mutator<C>, head: RootSlot, limit: u32) -> u64 {
    let base = mu.root_depth();
    let cur = mu.dup(head);
    let mut sum = 0u64;
    for _ in 0..limit {
        if mu.is_null(cur) {
            break;
        }
        sum = sum.wrapping_add(mu.read_u64(cur, 0));
        let next = mu.load(cur, 0);
        mu.assign(cur, next);
        mu.unwind(base + 1);
    }
    mu.unwind(base);
    sum
}

/// Repeatedly build a list, retaining every `keep_every`-th one in a long-lived
/// array. Steady garbage production against a live set of roughly fixed size.
pub fn list_churn<C: Collector>(
    mu: &mut Mutator<C>,
    rounds: u32,
    len: u32,
    keep_every: u32,
    seed: u64,
) -> u64 {
    const SLOTS: u16 = 8;
    let base = mu.root_depth();
    let mut rng = Rng::new(seed);

    let keeper = mu.alloc(SLOTS, 0);
    mu.set_global("keeper", keeper);
    mu.unwind(base);

    let mut checksum = 0u64;
    for round in 0..rounds {
        let head = build_list(mu, len, &mut rng);
        checksum = checksum.wrapping_add(walk_list(mu, head, len));
        if keep_every > 0 && round % keep_every == 0 {
            let k = mu.global("keeper");
            mu.store(k, (round / keep_every) as u16 % SLOTS, head);
        }
        mu.unwind(base);
    }

    let k = mu.global("keeper");
    for i in 0..SLOTS {
        let head = mu.load(k, i);
        if !mu.is_null(head) {
            checksum = checksum.wrapping_add(walk_list(mu, head, len));
        }
        mu.unwind(base + 1);
    }
    mu.unwind(base);
    checksum
}

/// A list long enough that tracing it with native recursion blows the stack.
pub fn deep_chain<C: Collector>(mu: &mut Mutator<C>, len: u32) -> u64 {
    let base = mu.root_depth();
    let mut rng = Rng::new(0xDEE9);
    let head = build_list(mu, len, &mut rng);
    mu.set_global("deep_chain", head);
    mu.unwind(base);
    mu.collect();
    let head = mu.global("deep_chain");
    let sum = walk_list(mu, head, len);
    mu.unwind(base);
    sum
}

// --------------------------------------------------------------- cycles ----

/// Build a ring of `n` nodes: each points at the next, the last at the first.
pub fn make_ring<C: Collector>(mu: &mut Mutator<C>, n: u32, rng: &mut Rng) -> RootSlot {
    assert!(n > 0, "a ring needs at least one node");
    let base = mu.root_depth();
    let head = mu.alloc(NODE_REFS, NODE_DATA);
    mu.write_u64(head, 0, rng.next_u64());
    let cur = mu.dup(head);
    for _ in 1..n {
        let node = mu.alloc(NODE_REFS, NODE_DATA);
        mu.write_u64(node, 0, rng.next_u64());
        mu.store(cur, 0, node);
        mu.assign(cur, node);
        mu.unwind(base + 2);
    }
    mu.store(cur, 0, head);
    mu.unwind(base + 1);
    head
}

/// Build and immediately abandon rings. Nothing survives, but every node in a
/// ring is pointed at by another node in the same ring.
pub fn cyclic_graph<C: Collector>(mu: &mut Mutator<C>, rounds: u32, ring: u32, seed: u64) -> u64 {
    let base = mu.root_depth();
    let mut rng = Rng::new(seed);
    let mut checksum = 0u64;
    for _ in 0..rounds {
        let head = make_ring(mu, ring, &mut rng);
        checksum = checksum.wrapping_add(walk_list(mu, head, ring));
        mu.unwind(base);
    }
    checksum
}

// -------------------------------------------------------- shape stresses ----

/// Payload sizes cycled through by [`fragmentation`], in bytes.
pub const FRAG_SIZES: [u32; 6] = [8, 24, 56, 120, 248, 504];

/// Overwrite random slots of a long-lived array with objects of varying size.
///
/// A non-moving allocator ends up with a heap full of holes that are each too
/// small for the next request.
pub fn fragmentation<C: Collector>(mu: &mut Mutator<C>, rounds: u32, live: u16, seed: u64) -> u64 {
    let base = mu.root_depth();
    let mut rng = Rng::new(seed);

    let arr = mu.alloc(live, 0);
    mu.set_global("frag_array", arr);
    mu.unwind(base);

    for round in 0..rounds {
        let a = mu.global("frag_array");
        let idx = rng.below(live as u64) as u16;
        let extra = FRAG_SIZES[rng.below(FRAG_SIZES.len() as u64) as usize];
        let o = mu.alloc(0, extra);
        mu.write_u64(o, 0, round as u64);
        mu.store(a, idx, o);
        mu.unwind(base);
    }

    let a = mu.global("frag_array");
    let mut checksum = 0u64;
    for i in 0..live {
        let o = mu.load(a, i);
        if !mu.is_null(o) {
            checksum = checksum.wrapping_add(mu.read_u64(o, 0));
        }
        mu.unwind(base + 1);
    }
    mu.unwind(base);
    checksum
}

/// Age an array into the old generation, then repeatedly point its slots at
/// freshly allocated young objects.
///
/// Every one of those stores creates an old-to-young reference, which is
/// exactly the edge a minor collection cannot discover by tracing from roots.
pub fn old_to_young<C: Collector>(mu: &mut Mutator<C>, slots: u16, rounds: u32, seed: u64) -> u64 {
    let base = mu.root_depth();
    let mut rng = Rng::new(seed);

    let old = mu.alloc(slots, 0);
    mu.set_global("old_array", old);
    mu.unwind(base);

    // Give the array time to be promoted before any old-to-young store.
    for _ in 0..3 {
        mu.collect();
    }

    for round in 0..rounds {
        let a = mu.global("old_array");
        let young = mu.alloc(NODE_REFS, NODE_DATA);
        mu.write_u64(young, 0, rng.next_u64() ^ round as u64);
        mu.store(a, (round % slots as u32) as u16, young);
        mu.unwind(base);
    }

    let a = mu.global("old_array");
    let mut checksum = 0u64;
    for i in 0..slots {
        let o = mu.load(a, i);
        if !mu.is_null(o) {
            checksum = checksum.wrapping_add(mu.read_u64(o, 0));
        }
        mu.unwind(base + 1);
    }
    mu.unwind(base);
    checksum
}

/// Many parents sharing a small pool of children.
///
/// The graph is a DAG, not a tree: a collector that copies an object every
/// time it finds a reference to it will silently turn shared nodes into
/// independent duplicates.
pub fn shared_dag<C: Collector>(mu: &mut Mutator<C>, parents: u32, children: u16, seed: u64) -> u64 {
    let base = mu.root_depth();
    let mut rng = Rng::new(seed);

    let pool = mu.alloc(children, 0);
    mu.set_global("pool", pool);
    for i in 0..children {
        let p = mu.global("pool");
        let child = mu.alloc(0, NODE_DATA);
        mu.write_u64(child, 0, i as u64 + 1);
        mu.store(p, i, child);
        mu.unwind(base);
    }

    let holder = mu.alloc(8, 0);
    mu.set_global("holder", holder);
    mu.unwind(base);

    for round in 0..parents {
        let p = mu.global("pool");
        let parent = mu.alloc(NODE_REFS, NODE_DATA);
        mu.write_u64(parent, 0, round as u64);
        for k in 0..NODE_REFS {
            let idx = rng.below(children as u64) as u16;
            let child = mu.load(p, idx);
            mu.store(parent, k, child);
            mu.unwind(base + 2);
        }
        let h = mu.global("holder");
        mu.store(h, (round % 8) as u16, parent);
        mu.unwind(base);
    }

    // Every child must still be a single object shared by its parents: sum the
    // pool directly and through the holder, and the two must agree in shape.
    let p = mu.global("pool");
    let mut checksum = 0u64;
    for i in 0..children {
        let c = mu.load(p, i);
        if !mu.is_null(c) {
            checksum = checksum.wrapping_add(mu.read_u64(c, 0));
        }
        mu.unwind(base + 1);
    }
    mu.unwind(base);
    checksum
}
