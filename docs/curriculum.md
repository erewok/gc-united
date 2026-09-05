# The curriculum

Eight modules, in order. Each is a complete collector of its kind, and each
exists partly to expose a limitation that the next one answers.

---

## 1 — Allocation

**`gc-mod01`** · bump pointers, free lists, splitting, coalescing, fragmentation

Before anything can be collected, something must hand out memory. This module
has no garbage collection at all: you call `free` yourself. It establishes the
object layout and the heap-walk invariant — *every block, live or free, carries
its true size* — that every sweeping collector later depends on.

The lesson at the end is fragmentation: a heap can hold plenty of free bytes and
still refuse a modest request, because no single hole is big enough. The
benchmark measures this directly. Modules 5 and 6 exist to make it go away.

Read: Wilson et al., *Dynamic Storage Allocation: A Survey and Critical Review*
(1995).

---

## 2 — Reference counting

**`gc-mod02`** · counts, retain/release ordering, cascading death, the cycle leak

Each object records how many references point at it; at zero it is dead,
immediately. Memory comes back promptly and there is no tracing phase, but the
mutator pays on every single reference assignment, and dropping the root of a
large structure frees all of it at once — a pause under another name.

Two traps are built in. The order of retain and release when overwriting a
reference matters only when the old and new values are the same object, which is
exactly the case a code generator produces constantly. And freeing an object
means releasing its children, which can cascade through a million objects, so it
cannot be done with native recursion.

The module ends by asserting its own limitation: abandoned cycles are never
reclaimed.

Read: Collins, *A method for overlapping and erasure of lists* (1960); Wilson,
*Uniprocessor Garbage Collection Techniques* (1992).

---

## 3 — Cycle collection

**`gc-mod03`** · trial deletion, candidate buffers, the four colours

Closes module 2's hole. A reference count says how many references point at an
object but not where from; for a garbage cycle, all of them come from inside.
So take a suspect subgraph and trially delete its internal references: whatever
is left at zero was only ever reachable from within, and is garbage.

Only objects whose count was decremented without reaching zero can be cycle
roots, so a collection examines suspects rather than the whole heap. This is,
in outline, what CPython's `gc` module does alongside its reference counting.

Read: Bacon and Rajan, *Concurrent Cycle Collection in Reference Counted
Systems* (2001); CPython's `Modules/gcmodule.c`.

---

## 4 — Mark and sweep

**`gc-mod04`** · tracing, reachability, explicit worklists, resetting per-cycle state

The course changes its mind about what garbage is. Instead of asking each object
how many references point at it, start from the roots and follow everything
reachable. What is left unmarked is unreachable, and unreachable is the real
definition of garbage. Cycles stop being a special case entirely — module 3's
machinery simply evaporates.

The costs invert too: marking is proportional to live data, sweeping to the size
of the whole heap. And the mark bit belongs to one collection, so a collector
that never clears it works perfectly exactly once.

Read: McCarthy, *Recursive Functions of Symbolic Expressions* (1960); Jones,
Hosking and Moss, *The Garbage Collection Handbook*, chapter 2.

---

## 5 — Mark-compact

**`gc-mod05`** · forwarding addresses, sliding objects, updating every reference

Mark-sweep leaves the heap full of holes. Compaction slides the survivors
together so free space is one contiguous run, which makes allocation a bump
pointer again and fragmentation impossible.

The price is that objects move, and this is the first module where they do.
Every reference to a moved object — in other objects, in the shadow stack, in
the global table — has to be found and rewritten. Miss one class of reference
and the heap still looks plausible; it just quietly refers to the wrong objects.
That is what the identity stamps in the verifier are for.

Read: *The Garbage Collection Handbook*, chapter 3; the Lisp 2 algorithm.

---

## 6 — Semispace copying

**`gc-mod06`** · from-space and to-space, forwarding pointers, Cheney's algorithm

Split the heap in two, use one half, and when it fills, copy the live objects
into the other half and swap. Cost is proportional to what *survives*, not to
the size of the heap, so a program that produces mostly garbage is collected
almost for free. Allocation is a bump pointer and compaction is automatic.

Half the memory is unused at all times, and every surviving object is copied on
every collection. The defining hazard is sharing: an object reachable by two
paths must be copied once and both references updated to the same address. A
collector that copies it twice produces a heap that looks perfect and has
silently turned one object into two.

Read: Cheney, *A nonrecursive list compacting algorithm* (1970); Fenichel and
Yochelson (1969).

---

## 7 — Generational collection

**`gc-mod07`** · the weak generational hypothesis, write barriers, remembered sets

Almost all objects die young. So collect the young ones often and cheaply, and
leave the old ones alone. This one observation is why production collectors are
fast, and modules 5 and 6 are the machinery it is built from.

It creates a problem that cannot be solved by tracing: an old object pointing at
a young one is a reference a minor collection will never discover, because it
never looks at the old generation. The mutator has to report those references as
it creates them, through a **write barrier** — the first place in this course
where the collector imposes a cost on ordinary program code.

Read: Lieberman and Hewitt (1983); Ungar, *Generation Scavenging* (1984);
Appel, *Simple generational garbage collection and fast allocation* (1989).

---

## 8 — Incremental tri-colour marking

**`gc-mod08`** · white/grey/black, the tri-colour invariant, barriers, floating garbage

Every collector so far stops the program for the whole collection. This one
marks a little at a time and lets the mutator run in between — which means the
graph changes underneath the marker.

The tri-colour abstraction makes the danger precise. An object is white
(unreached), grey (reached, not yet scanned) or black (scanned). Collection is
safe as long as no black object ever points to a white one with no grey path
protecting it. The mutator can violate that with a single store, so a write
barrier restores it: Dijkstra's shades the new value, Yuasa's shades the
overwritten one.

The cost is precision. Objects that die after being marked survive to the next
cycle — floating garbage — and objects allocated mid-cycle need a colour chosen
carefully enough that they are not swept the moment they are born.

Read: Dijkstra et al., *On-the-fly garbage collection* (1978); Yuasa (1990);
Go's `runtime/mgc.go`.

---

## Where the real systems sit

- **CPython** — modules 2 and 3: reference counting for everything, plus a
  generational cycle detector for the rest.
- **HotSpot (serial/parallel)** — modules 6 and 7 for the young generation,
  module 5 for the old one.
- **V8** — module 7, with a copying scavenger for the nursery and incremental
  marking (module 8) for the old space.
- **Go** — module 8: concurrent tri-colour marking with a deletion barrier, and
  no generations at all.
- **Boehm** — module 4, conservatively: it does not know which words are
  pointers, so it treats anything that looks like one as a root.
