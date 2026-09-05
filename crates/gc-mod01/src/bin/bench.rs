//! Module 1 benchmark.
//!
//! There is no collector here, so what matters is allocator throughput and the
//! shape of the heap it leaves behind. The fragmentation column is the one to
//! watch: it is the largest single free block as a fraction of all free bytes.
//! At 100% the free space is one contiguous run and any object that fits in
//! the total will fit; as it falls, the heap fills with holes that are
//! individually too small to use.

use std::time::{Duration, Instant};

use gc_core::heap::{HEADER_SIZE, Handle};
use gc_mod01::{Allocator, FitPolicy};

const HEAP: usize = 4 << 20;

struct Row {
    name: &'static str,
    policy: FitPolicy,
    elapsed: Duration,
    allocations: u64,
    failures: u64,
    peak_used: u32,
    free_bytes: u32,
    largest_free: u32,
    splits: u64,
    merges: u64,
    reused: u64,
}

fn rng(state: &mut u64) -> u64 {
    *state ^= *state << 13;
    *state ^= *state >> 7;
    *state ^= *state << 17;
    *state
}

/// Fill the heap with cells and never free: pure bump-pointer throughput.
fn bump_only(a: &mut Allocator) -> u32 {
    let mut peak = 0;
    while a.alloc(0, 48).is_some() {
        peak = peak.max(a.used_bytes());
    }
    peak
}

/// Allocate a generation, free it entirely, repeat. Every round should reuse
/// the same memory.
fn generational_churn(a: &mut Allocator) -> u32 {
    let mut peak = 0;
    let mut live = Vec::with_capacity(4096);
    for _ in 0..64 {
        for _ in 0..4096 {
            match a.alloc(2, 40) {
                Some(h) => live.push(h),
                None => break,
            }
        }
        peak = peak.max(a.used_bytes());
        for h in live.drain(..) {
            a.free(h);
        }
        a.coalesce_all();
    }
    peak
}

/// Mixed sizes with random frees and no coalescing: the pathological case for
/// a non-moving allocator.
fn fragmenting_churn(a: &mut Allocator) -> u32 {
    let mut seed = 0x5EEDu64;
    let mut live: Vec<Handle> = Vec::new();
    let mut peak = 0;

    for _ in 0..200_000 {
        if !live.is_empty() && rng(&mut seed) % 100 < 48 {
            let idx = (rng(&mut seed) % live.len() as u64) as usize;
            let h = live.swap_remove(idx);
            a.free(h);
        } else {
            let ndata = 8 + (rng(&mut seed) % 24) as u32 * 16;
            if let Some(h) = a.alloc((rng(&mut seed) % 4) as u16, ndata) {
                live.push(h);
                peak = peak.max(a.used_bytes());
            }
        }
    }
    peak
}

fn run(name: &'static str, policy: FitPolicy, f: fn(&mut Allocator) -> u32) -> Row {
    let mut a = Allocator::with_policy(HEAP, policy);
    let start = Instant::now();
    let peak_used = f(&mut a);
    let elapsed = start.elapsed();

    if let Err(e) = a.validate() {
        eprintln!("  {name} ({policy:?}) left the heap in an inconsistent state: {e}");
    }
    let s = a.stats().clone();
    Row {
        name,
        policy,
        elapsed,
        allocations: s.allocations,
        failures: s.failures,
        peak_used,
        free_bytes: a.free_bytes(),
        largest_free: a.largest_free_block(),
        splits: s.splits,
        merges: s.merges,
        reused: s.reused,
    }
}

fn main() {
    let rows = vec![
        run("bump only", FitPolicy::First, bump_only),
        run("generational", FitPolicy::First, generational_churn),
        run("fragmenting", FitPolicy::First, fragmenting_churn),
        run("fragmenting", FitPolicy::Best, fragmenting_churn),
    ];

    println!();
    println!("  benchmark: module 1 allocator, {} MiB heap", HEAP >> 20);
    println!();
    println!(
        "  {:<14} {:>6} {:>9} {:>11} {:>10} {:>9} {:>10} {:>8} {:>8} {:>8}",
        "workload", "fit", "wall ms", "M allocs/s", "refused", "peak MiB", "unfragm.", "splits",
        "merges", "reused"
    );
    println!("  {}", "-".repeat(108));

    for r in &rows {
        let ms = r.elapsed.as_secs_f64() * 1e3;
        let rate = if ms > 0.0 { r.allocations as f64 / (ms * 1e3) } else { 0.0 };
        let unfragmented = if r.free_bytes > 0 {
            r.largest_free as f64 / r.free_bytes as f64 * 100.0
        } else {
            100.0
        };
        println!(
            "  {:<14} {:>6} {:>9.2} {:>11.2} {:>10} {:>9.2} {:>9.1}% {:>8} {:>8} {:>8}",
            r.name,
            format!("{:?}", r.policy).to_lowercase(),
            ms,
            rate,
            r.failures,
            r.peak_used as f64 / (1024.0 * 1024.0),
            unfragmented,
            r.splits,
            r.merges,
            r.reused,
        );
    }
    println!();
    println!(
        "  unfragm. = largest free block as a share of all free bytes; \
         100% means one contiguous run"
    );
    println!(
        "  a request for {} bytes needs a single hole that big, however many free bytes exist",
        HEADER_SIZE + 48
    );
    println!();
}
