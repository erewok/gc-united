# GC United

A garbage collection course in Rust, taught by debugging.

Note: almost everything in here was contributed by Claude code (except for this sentence and some others). This is part of my own rumination on the idea that I might be able to get Claude to help me recover the "productive failures" I've experienced in my career while trying to learn stuff. In short, what if Claude could produce broken or partial things so that I can have some friction trying to fix these? Would this result in learning and retention for _me_ (I don't want to ship code and not understand it, but it's looking like a pretty common approach...). So treat this whole thing as an experiment. It's not meant to be some huge celebration of LLMs building stuff: instead, it's _intentionally_ broken in order to pursue _human learning objectives_ (my own).

Every module in `crates/` is a working-ish garbage collector that does **not**
pass its tests. Some functions are unimplemented and panic with `todo!()`;
elsewhere the code is complete, plausible, and just plain incorrect! Our job is to make the tests pass.

```
just list        the modules
just run 1       run module 1's tests, then its benchmark if they all pass
just test 4      just the tests
just bench 4     just the benchmark
just test-all    every module
```

## Requirements

- Rust 1.97 or later
- [`just`](https://github.com/casey/just)
- [`cargo-nextest`](https://nexte.st) — `cargo install cargo-nextest`

nextest is not optional. It runs each test in its own process, so one failing
test cannot take the rest of the suite down with it, and you get a full picture
of what works and what does not after every change.

## How a module works

Open `crates/gc-modNN/src/lib.rs`. The module documentation at the top explains
the paradigm, what the collector is supposed to do, and which functions are
unimplemented. We have happily left some fun bugs in these modules also!

Suggested flow: run the tests, read the failures, which are written to describe the *state of the heap*, but necessarily not the line of code at fault:

```
collection 2 left 98208 bytes in use where collection 1 left 49104, and the
live set is the same shape and size every round.
```

That is a fact about what your collector did and working out which line caused it
is the exercise. When every test passes, the benchmark runs and you get numbers
to compare against the other modules.

## Foundational Provided Things

`crates/gc-core` is foundational, not intended as an exercise. It _should be_ correct (but this is software...), and should not require any changes.

**`heap`** — a flat byte arena. Objects are a 16-byte header followed by
reference slots and scalar payload, aligned to 16 bytes. A `Handle` is a byte
offset, so a collector that moves an object invalidates every handle to it.

**`roots`** — the shadow stack and the global table. Mutator code holds a
`RootSlot`, which is an index, never a raw `Handle`, so a relocating collector
can rewrite the addresses underneath it. Real runtimes do exactly this.

**`space`** — a correct free-list heap, used by the collectors that do not move
objects. Allocation is module 1's subject; after that it is scaffolding.

**`mutator`** — the API workloads are written against. It maintains an
independent model of the object graph in lockstep with the real heap.

**`verify`** — the part that makes "tests are the specification" workable.
Verification never asks the collector what it thinks is alive. It walks the real
heap from the real roots and compares what it finds against the model. Every
object carries an identity stamp in its payload, so the walk can tell "the
object that should be here" from "some other object that is here now" — which is
the difference between a relocating collector that works and one that merely
looks like it does.

`crates/gc-workloads` holds the candidate programs, and `crates/gc-harness` the
conformance checks and the benchmark runner. These are the things that we evaluate the different GC implementations on.

Each module's test file names the checks that apply to it, so the suite reads as a statement of what that collector promises. For example, module 2 deliberately does not claim to collect cycles, and asserts that it leaks them.

## The modules

| # | crate | paradigm | in the wild |
|---|-------|----------|-------------|
| 1 | `gc-mod01` | bump allocation and free lists | every allocator underneath everything else |
| 2 | `gc-mod02` | reference counting | CPython, Swift ARC, `Rc`, `shared_ptr` |
| 3 | `gc-mod03` | cycle collection by trial deletion | CPython's `gc` module |
| 4 | `gc-mod04` | mark and sweep | Boehm, early Ruby, most old generations |
| 5 | `gc-mod05` | mark-compact | HotSpot's serial old generation |
| 6 | `gc-mod06` | semispace copying | young generations everywhere |
| 7 | `gc-mod07` | generational collection | HotSpot, V8, .NET |
| 8 | `gc-mod08` | incremental tri-colour marking | Go, V8, Shenandoah's marking |

Work them in order. Each one assumes the ideas of the ones before it, and
several are built specifically to fail in ways the previous module could not.

`docs/curriculum.md` goes into what each module is teaching and where to read
more.

## Reading the benchmark

Wall-clock time on its own says very little about a collector — the fastest
collector is the one that never runs, right up to the moment it exhausts memory.
So every benchmark reports space alongside time: how much was allocated, how
much survived, how much of the run was spent collecting, and the longest single
pause. A row that fails verification is reported as such, and its timings are
meaningless.

## A note on this toolchain

Panic unwinding appears to be broken in some local Rust installations on macOS:
a failing assertion aborts the process with `failed to initiate panic, error 5`
instead of unwinding. This is not caused by anything in this repository — a
two-line crate reproduces it. Under nextest it is harmless, because each test
already runs in its own process and an abort is reported as that test failing,
with its assertion message intact. It is another reason plain `cargo test` is
not the supported way to run these suites.
