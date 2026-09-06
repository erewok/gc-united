//! Benchmark runner.
//!
//! Wall-clock time alone is a poor way to judge a collector: the fastest
//! collector is always the one that never collects, right up until it runs out
//! of memory. So every row reports time *and* space — how much was allocated,
//! how much survived, how much of the run was spent inside the collector, and
//! the longest single pause.
//!
//! Every workload is verified after it finishes. A row that fails verification
//! is reported as such and its timings are meaningless.

use std::time::{Duration, Instant};

use gc_core::collector::Collector;
use gc_core::mutator::Mutator;
use gc_core::stats::GcStats;

/// A candidate program: run it against a collector, get its checksum back.
pub type Workload<C> = Box<dyn Fn(&mut Mutator<C>) -> u64>;

pub struct WorkloadSpec<C: Collector> {
    pub name: &'static str,
    pub heap_bytes: usize,
    pub run: Workload<C>,
}

/// Describe one benchmark: a name, the heap it runs in, and the program.
pub fn spec<C: Collector>(
    name: &'static str,
    heap_bytes: usize,
    run: impl Fn(&mut Mutator<C>) -> u64 + 'static,
) -> WorkloadSpec<C> {
    WorkloadSpec {
        name,
        heap_bytes,
        run: Box::new(run),
    }
}

pub struct BenchRow {
    pub name: &'static str,
    pub heap_bytes: usize,
    pub wall: Duration,
    pub checksum: u64,
    pub stats: GcStats,
    pub live_bytes: u32,
    pub verified: bool,
    pub failure: Option<String>,
}

/// Run each workload against a freshly constructed collector.
pub fn run_bench<C: Collector>(
    mk: impl Fn(usize) -> C,
    specs: Vec<WorkloadSpec<C>>,
) -> Vec<BenchRow> {
    specs
        .into_iter()
        .map(|s| {
            let mut mu = Mutator::new(mk(s.heap_bytes));
            let start = Instant::now();
            let checksum = (s.run)(&mut mu);
            let wall = start.elapsed();

            mu.forget_unreachable();
            let report = mu.verify();
            let verified = report.ok();
            let failure = (!verified).then(|| report.to_string());

            BenchRow {
                name: s.name,
                heap_bytes: s.heap_bytes,
                wall,
                checksum,
                stats: mu.stats().clone(),
                live_bytes: mu.used_bytes(),
                verified,
                failure,
            }
        })
        .collect()
}

fn mib(bytes: u64) -> f64 {
    bytes as f64 / (1024.0 * 1024.0)
}

/// Print the benchmark table. Returns false if any workload failed verification.
pub fn print_table(collector: &str, rows: &[BenchRow]) -> bool {
    println!();
    println!("  benchmark: {collector}");
    println!();
    println!(
        "  {:<16} {:>9} {:>10} {:>9} {:>6} {:>9} {:>6} {:>11} {:>10}",
        "workload",
        "wall ms",
        "alloc MiB",
        "objects",
        "GCs",
        "in gc ms",
        "gc %",
        "max pause",
        "live MiB"
    );
    println!("  {}", "-".repeat(96));

    let mut all_ok = true;
    for r in rows {
        let wall_ms = r.wall.as_secs_f64() * 1e3;
        let gc_ms = r.stats.gc_time.as_secs_f64() * 1e3;
        let gc_pct = if wall_ms > 0.0 {
            gc_ms / wall_ms * 100.0
        } else {
            0.0
        };
        let pause = r.stats.max_pause;
        let pause_str = if pause.as_micros() < 10_000 {
            format!("{} us", pause.as_micros())
        } else {
            format!("{:.1} ms", pause.as_secs_f64() * 1e3)
        };
        println!(
            "  {:<16} {:>9.2} {:>10.2} {:>9} {:>6} {:>9.2} {:>5.1}% {:>11} {:>10.2}",
            r.name,
            wall_ms,
            mib(r.stats.bytes_allocated),
            r.stats.allocations,
            r.stats.collections,
            gc_ms,
            gc_pct,
            pause_str,
            mib(r.live_bytes as u64),
        );
        if !r.verified {
            all_ok = false;
        }
    }

    println!();
    for r in rows.iter().filter(|r| !r.verified) {
        println!("  {} FAILED VERIFICATION", r.name);
        for line in r.failure.as_deref().unwrap_or("").lines() {
            println!("    {line}");
        }
        println!();
    }

    if all_ok {
        println!("  all workloads verified; heap stayed consistent throughout");
    } else {
        println!("  timings above are not meaningful: the heap did not survive the run");
    }
    println!();
    all_ok
}

/// Run and print, exiting non-zero if any workload failed verification.
pub fn main_bench<C: Collector>(
    collector: &str,
    mk: impl Fn(usize) -> C,
    specs: Vec<WorkloadSpec<C>>,
) {
    let rows = run_bench(mk, specs);
    if !print_table(collector, &rows) {
        std::process::exit(1);
    }
}
