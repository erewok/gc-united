# Reading, mapped to the modules

`docs/curriculum.md` names one or two sources per module. This is the longer version: what to read, an ordering of stuff to read, and *what to read it for*. The goal is reading to recognise what's in this repo.

For a book instead of thirty papers, there's also Jones, Hosking and Moss, *The Garbage Collection Handbook*; the chapter pointers below assume the 2nd edition (2016) or later.

---

## 1 — Allocation · `gc-mod01`

**Primary.** Wilson, Johnstone, Neely and Boles, *Dynamic Storage Allocation: A Survey and Critical Review* (1995).

This paper includes an opinionated survey as well as a description of fragmentation problems and they offer a critique of synthetic benchmarks. The authors' central claim is that most allocator evaluations of the era were problematic because randomly generated allocation traces do not resemble real programs. (We tried to measure *real* workload in our benchmarks for this project rather than a random one in order to avoid this problem.)

**Then.** Knuth, *The Art of Computer Programming* Vol. 1, §2.5. First-fit versus best-fit, and **boundary tags** — Knuth's term for the invariant that module 1 establishes and every later sweeping module depends on: every block,
live or free, carries its own size, so the heap can be walked linearly.

**Implementation.** `dlmalloc` (Doug Lea) — the allocator most others descend from. Lea's own notes on it are short and readable.

---

## 2 — Reference counting · `gc-mod02`

**Primary.** Collins, *A method for overlapping and erasure of lists* (1960).
A one-pager on the origin of reference counting, and it already notes the cycle problem you assert at the end of the module.

**Then.** Wilson, *Uniprocessor Garbage Collection Techniques* (1992) §2. The best survey of the field, and the clearest statement of why reference counting loses on throughput: the mutator pays on every pointer assignment.

Deutsch and Bobrow, *An efficient, incremental, automatic garbage collector* (1976) — **deferred reference counting**, which removes the cost on local variable updates. This is the fix for the reference counting problems we have in our module.

**Implementation.** Swift's ARC, and CPython's `Py_INCREF`/`Py_DECREF`. Both show the retain-before-release ordering.

---

## 3 — Cycle collection · `gc-mod03`

**Primary.** Bacon and Rajan, *Concurrent Cycle Collection in Reference Counted Systems* (2001). This is the paper we tried to implement in in this module.

**Then.** Lins, *Cyclic reference counting with lazy mark-sweep* (1992), the direct ancestor.

**Implementation.** CPython `Modules/gcmodule.c`. This is a generational cycle detector sitting on top of reference counting, which is a combination of our modules 2, 3 and 7.

---

## 4 — Mark and sweep · `gc-mod04`

**Primary.** McCarthy, *Recursive Functions of Symbolic Expressions and Their Computation by Machine, Part I* (1960). Garbage collection is introduced almost
in passing, near the end, as an implementation detail of LISP. Worth reading for how casually the field's founding idea arrives.

**Then.** *The GC Handbook* ch. 2. Then Boehm and Weiser, *Garbage collection in an uncooperative environment* (1988) — **conservative** collection, where the collector cannot tell a pointer from an integer and must treat anything plausible as a root. It is module 4 but much more difficult.

**Implementation.** The Boehm–Demers–Weiser collector.

---

## 5 — Mark-compact · `gc-mod05`

**Primary.** *The GC Handbook* ch. 3. The Lisp 2 sliding algorithm we tried to implement doesn't appear in a single paper, but the Handbook's treatment is the canonical modern write-up.

**Then.** Cohen and Nicolau, *Comparison of compacting algorithms for garbage collection* (1983) — compares Lisp 2 against threaded and table-based compaction, and explains why Lisp 2's extra forwarding word is usually worth it.

Jonkers, *A fast garbage compaction algorithm* (1979) — **threaded compaction**, which stores forwarding information inside the reference chains themselves and needs no extra word at all. A genuinely clever trick, and much easier to appreciate once you have written the straightforward version.

**Implementation.** HotSpot's Serial Old collector works in the same way.

---

## 6 — Semispace copying · `gc-mod06`

**Primary.** Cheney, *A nonrecursive list compacting algorithm* (1970). A short (two pages!) paper which we tried to implement here. While reaidng, look for the `scan`/`free` cursor trick stated in its original form.

**Then.** Fenichel and Yochelson, *A LISP garbage-collector for virtual-memory computer systems* (1969), which is the recursive copying collector Cheney's paper improves on.

Baker, *List processing in real time on a serial computer* (1978) — **incremental** copying with a **read barrier**. This is the single most important paper for understanding modern low-pause collectors, and it is the bridge from module 6 to ZGC and Shenandoah.

---

## 7 — Generational collection · `gc-mod07`

**Primary.** Ungar, *Generation Scavenging: A non-disruptive high performance storage reclamation algorithm* (1984). Eden, survivor spaces and tenuring, which is the shape HotSpot's young generation still has.

**Then.** Lieberman and Hewitt, *A real-time garbage collector based on the lifetimes of objects* (1983) — the first generational collector, and where the **weak generational hypothesis** is argued rather than assumed.

Appel, *Simple generational garbage collection and fast allocation* (1989) — this the design that most inpsired this section, and the source of the "allocation is a pointer bump" argument.

Hölzle, *A fast write barrier for generational garbage collectors* (1993) — **card marking**, the write barrier real collectors use instead of the remembered-set list we have in here.

For the other side of the argument: Blackburn et al., *Myths and realities: the performance impact of garbage collection* (2004), which measures how much the generational hypothesis actually buys.

**Implementation.** HotSpot's ParNew/Serial young generation; V8's scavenger.

---

## 8 — Incremental tri-colour marking · `gc-mod08`

**Primary.** Dijkstra, Lamport, Martin, Scholten and Steffens, *On-the-fly garbage collection: an exercise in cooperation* (1978). The tri-colour abstraction and the **incremental-update** barrier we tried to implement here. It is written as a concurrency proof, so read it for the invariant, not the code.

**Then.** Yuasa, *Real-time garbage collection on general-purpose machines* (1990) — the **snapshot-at-the-beginning** (deletion) barrier, which shades the *overwritten* value instead of the stored one. Your module's brief names it; the JVM's G1 and Shenandoah use it. It is a genuinely valid alternative for the collector we have.

Steele, *Multiprocessing compactifying garbage collection* (1975), an earlier and more difficult paper.

**Implementation.** Go's `runtime/mgc.go`. The comment block at the top of that file is one of the best pieces of GC writing anywhere, and Go's collector is tri-colour with a deletion barrier and no generations at all.

---

## A short path through it

To survey the field rather than get lost in the weeds, a valid path would potentially be this order:

1. Cheney (1970) — two pages
2. McCarthy (1960), the GC section only
3. Wilson (1992) — the survey; read §2–3 closely
4. Dijkstra et al. (1978) — the invariant
5. Ungar (1984) — the hypothesis that made GC fast
6. Detlefs et al. (2004) — where production JVMs actually are
7. Baker (1978) — where the low-pause collectors come from

---

*Venues and years above are given as author-title-year so the sources are easy
to find; a few of the exact publication venues were written from memory and are
worth confirming before citing formally — in particular Hölzle (1993), Cohen and
Nicolau (1983), and the Shenandoah and C4 papers.*
