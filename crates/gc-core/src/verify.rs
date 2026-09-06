//! Independent verification of a collector's behaviour.
//!
//! Verification never asks the collector what it thinks is alive. It walks the
//! *real* heap through the collector's own `read_field`, starting from the
//! collector's own roots, and compares what it finds against the model the
//! mutator built. Every object carries an identity stamp, so the walk can tell
//! "the right object" from "some other object that now occupies this address".
//!
//! That distinction is the whole point. A relocating collector that forgets to
//! update one pointer usually still yields a *plausible* heap; only the stamps
//! reveal that a field now names the wrong object.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::fmt;

use crate::collector::Collector;
use crate::heap::Handle;
use crate::model::{ID_OFF, MAGIC, MAGIC_OFF, Model, STAMP_BYTES};

/// Where a handle was found, so failures can name a path instead of an address.
#[derive(Clone, Debug)]
pub enum Origin {
    RootSlot(usize),
    Global(String),
    Field { parent: u64, index: u16 },
}

impl fmt::Display for Origin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Origin::RootSlot(i) => write!(f, "root slot {i}"),
            Origin::Global(n) => write!(f, "global {n:?}"),
            Origin::Field { parent, index } => write!(f, "object #{parent} field [{index}]"),
        }
    }
}

#[derive(Clone, Debug)]
pub enum Violation {
    /// A handle does not address a well-formed object inside the arena.
    OutOfBounds { origin: Origin, handle: Handle },
    /// The object at this handle has no valid identity stamp: it was freed,
    /// overwritten, or was never an object.
    CorruptPayload {
        origin: Origin,
        handle: Handle,
        found: u32,
    },
    /// The stamp is valid but names an object the model has never seen.
    UnknownIdentity {
        origin: Origin,
        handle: Handle,
        id: u64,
    },
    /// Two different addresses claim to be the same object.
    DuplicatedIdentity { id: u64, at: Vec<Handle> },
    /// The object at this handle is not the one that should be here.
    WrongObject {
        origin: Origin,
        handle: Handle,
        expected: u64,
        found: u64,
    },
    /// A slot that should be null is not, or vice versa.
    NullMismatch { origin: Origin, expected_null: bool },
    /// The object's shape changed underneath the mutator.
    ShapeMismatch {
        id: u64,
        handle: Handle,
        expected_nrefs: u16,
        found_nrefs: u16,
    },
    /// Reachable according to the model, but the walk never reached it.
    Unreachable { id: u64, path: String },
    /// The root stack and the model's mirror of it have diverged.
    RootStackDepth { collector: usize, model: usize },
    /// A global the model holds is missing from the collector's root table.
    MissingGlobal { name: String },
    /// Bytes in use do not match the bytes the model says are still reachable.
    Accounting {
        used: u32,
        reachable: u64,
        unreclaimed: i64,
    },
}

impl fmt::Display for Violation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Violation::OutOfBounds { origin, handle } => write!(
                f,
                "{origin} holds {handle:?}, which is not a well-formed object address"
            ),
            Violation::CorruptPayload {
                origin,
                handle,
                found,
            } => write!(
                f,
                "{origin} holds {handle:?}, but the object there has no identity stamp \
                 (magic {found:#x}, expected {MAGIC:#x}) — it is reachable memory that has \
                 been freed or overwritten"
            ),
            Violation::UnknownIdentity { origin, handle, id } => {
                write!(
                    f,
                    "{origin} holds {handle:?}, stamped with unknown identity #{id}"
                )
            }
            Violation::DuplicatedIdentity { id, at } => write!(
                f,
                "object #{id} exists at {at:?} — one object now has {} distinct copies, \
                 so mutating one will not be seen through the other",
                at.len()
            ),
            Violation::WrongObject {
                origin,
                handle,
                expected,
                found,
            } => write!(
                f,
                "{origin} should name object #{expected}, but {handle:?} holds object #{found}"
            ),
            Violation::NullMismatch {
                origin,
                expected_null,
            } => {
                if *expected_null {
                    write!(f, "{origin} should be null but holds an object")
                } else {
                    write!(f, "{origin} should hold an object but is null")
                }
            }
            Violation::ShapeMismatch {
                id,
                handle,
                expected_nrefs,
                found_nrefs,
            } => write!(
                f,
                "object #{id} at {handle:?} was allocated with {expected_nrefs} reference \
                 slots but its header now claims {found_nrefs}"
            ),
            Violation::Unreachable { id, path } => write!(
                f,
                "object #{id} is still reachable from the roots ({path}) but the walk never \
                 arrived at it — it was collected while live, or a pointer to it was lost"
            ),
            Violation::RootStackDepth { collector, model } => write!(
                f,
                "the collector's shadow stack is {collector} deep, the mutator pushed {model}"
            ),
            Violation::MissingGlobal { name } => {
                write!(
                    f,
                    "global {name:?} is bound in the mutator but absent from the roots"
                )
            }
            Violation::Accounting {
                used,
                reachable,
                unreclaimed,
            } => {
                if *unreclaimed > 0 {
                    write!(
                        f,
                        "{used} bytes still in use but only {reachable} are reachable: \
                         {unreclaimed} bytes of garbage survived collection"
                    )
                } else {
                    write!(
                        f,
                        "{used} bytes in use but {reachable} are reachable: the collector is \
                         under-reporting by {} bytes",
                        -unreclaimed
                    )
                }
            }
        }
    }
}

/// Most violations recorded before the rest are counted rather than described.
///
/// A collector that loses one object produces one violation; a collector that
/// loses the whole heap produces one per object, and describing every one of
/// them costs more than the collection did.
const MAX_VIOLATIONS: usize = 64;

/// How many lost objects get a root path worked out for them. Each path costs
/// a walk of the model, so only the ones that will actually be printed are
/// worth computing.
const MAX_PATHS: usize = 8;

/// The outcome of a verification pass.
#[derive(Default)]
pub struct Report {
    pub violations: Vec<Violation>,
    /// Violations beyond [`MAX_VIOLATIONS`], counted but not described.
    pub omitted: usize,
    /// Distinct objects the walk reached.
    pub visited: usize,
}

impl Report {
    pub fn ok(&self) -> bool {
        self.violations.is_empty() && self.omitted == 0
    }

    fn record(&mut self, v: Violation) {
        if self.violations.len() < MAX_VIOLATIONS {
            self.violations.push(v);
        } else {
            self.omitted += 1;
        }
    }

    fn full(&self) -> bool {
        self.violations.len() >= MAX_VIOLATIONS
    }
}

impl fmt::Display for Report {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.ok() {
            return write!(f, "heap is consistent ({} objects reached)", self.visited);
        }
        let total = self.violations.len() + self.omitted;
        writeln!(
            f,
            "{}{} heap invariant violation(s) after reaching {} objects:",
            if self.omitted > 0 { "at least " } else { "" },
            total,
            self.visited
        )?;
        for (n, v) in self.violations.iter().enumerate().take(12) {
            writeln!(f, "  {}. {v}", n + 1)?;
        }
        if total > 12 {
            writeln!(f, "  ... and {} more", total - 12)?;
        }
        Ok(())
    }
}

/// Read the identity stamp of the object at `h`, if it has one.
fn identity<C: Collector>(gc: &C, h: Handle, origin: &Origin) -> Result<u64, Violation> {
    if !gc.heap().in_bounds(h) {
        return Err(Violation::OutOfBounds {
            origin: origin.clone(),
            handle: h,
        });
    }
    if (gc.heap().data(h).len() as u32) < STAMP_BYTES {
        return Err(Violation::CorruptPayload {
            origin: origin.clone(),
            handle: h,
            found: 0,
        });
    }
    let magic = gc.heap().data_u32(h, MAGIC_OFF);
    if magic != MAGIC {
        return Err(Violation::CorruptPayload {
            origin: origin.clone(),
            handle: h,
            found: magic,
        });
    }
    Ok(gc.heap().data_u64(h, ID_OFF))
}

/// Render the model's shortest root path to `id`, for diagnostics.
fn path_to(model: &Model, target: u64) -> String {
    let mut prev: HashMap<u64, (Origin, u64)> = HashMap::new();
    let mut seen: HashSet<u64> = HashSet::new();
    let mut queue: Vec<u64> = Vec::new();

    for (slot, id) in (0..model.root_depth()).filter_map(|s| model.root(s).map(|i| (s, i))) {
        if seen.insert(id) {
            prev.insert(id, (Origin::RootSlot(slot), 0));
            queue.push(id);
        }
    }
    for (name, &id) in model.globals() {
        if seen.insert(id) {
            prev.insert(id, (Origin::Global(name.clone()), 0));
            queue.push(id);
        }
    }

    let mut head = 0;
    while head < queue.len() {
        let id = queue[head];
        head += 1;
        if id == target {
            break;
        }
        let Some(o) = model.get(id) else { continue };
        for (i, child) in o.fields.iter().enumerate() {
            if let Some(c) = child
                && seen.insert(*c)
            {
                prev.insert(
                    *c,
                    (
                        Origin::Field {
                            parent: id,
                            index: i as u16,
                        },
                        id,
                    ),
                );
                queue.push(*c);
            }
        }
    }

    if !prev.contains_key(&target) {
        return "no path found".to_string();
    }
    let mut steps = Vec::new();
    let mut cur = target;
    loop {
        let (origin, parent) = prev[&cur].clone();
        match origin {
            Origin::Field { index, .. } => steps.push(format!("[{index}]")),
            other => {
                steps.push(other.to_string());
                break;
            }
        }
        cur = parent;
    }
    steps.reverse();
    steps.join(" -> ")
}

/// Walk the real heap from the real roots and compare it against the model.
pub fn verify<C: Collector>(gc: &mut C, model: &Model) -> Report {
    let mut report = Report::default();
    let mut id_at: HashMap<u64, Vec<Handle>> = HashMap::new();
    let mut visited: HashSet<Handle> = HashSet::new();
    let mut queue: Vec<(Handle, Origin, Option<u64>)> = Vec::new();

    if gc.roots().depth() != model.root_depth() {
        report.record(Violation::RootStackDepth {
            collector: gc.roots().depth(),
            model: model.root_depth(),
        });
    }

    let depth = gc.roots().depth().min(model.root_depth());
    for slot in 0..depth {
        let h = gc.roots().get(crate::roots::RootSlot(slot));
        let expected = model.root(slot);
        match (expected, h.is_null()) {
            (None, true) => {}
            (None, false) => report.record(Violation::NullMismatch {
                origin: Origin::RootSlot(slot),
                expected_null: true,
            }),
            (Some(_), true) => report.record(Violation::NullMismatch {
                origin: Origin::RootSlot(slot),
                expected_null: false,
            }),
            (Some(id), false) => queue.push((h, Origin::RootSlot(slot), Some(id))),
        }
    }

    for (name, &id) in model.globals() {
        let h = gc.roots().global(name);
        if h.is_null() {
            report.record(Violation::MissingGlobal { name: name.clone() });
        } else {
            queue.push((h, Origin::Global(name.clone()), Some(id)));
        }
    }

    while let Some((h, origin, expected)) = queue.pop() {
        let found = match identity(gc, h, &origin) {
            Ok(id) => id,
            Err(v) => {
                report.record(v);
                continue;
            }
        };
        if let Some(want) = expected
            && want != found
        {
            report.record(Violation::WrongObject {
                origin: origin.clone(),
                handle: h,
                expected: want,
                found,
            });
        }
        let Some(m) = model.get(found) else {
            report.record(Violation::UnknownIdentity {
                origin,
                handle: h,
                id: found,
            });
            continue;
        };

        id_at.entry(found).or_default().push(h);
        if !visited.insert(h) {
            continue;
        }

        let nrefs = gc.heap().nrefs(h);
        if nrefs != m.nrefs {
            report.record(Violation::ShapeMismatch {
                id: found,
                handle: h,
                expected_nrefs: m.nrefs,
                found_nrefs: nrefs,
            });
            continue;
        }

        let expected_fields = m.fields.clone();
        for (i, want) in expected_fields.iter().enumerate() {
            let i = i as u16;
            let child = gc.read_field(h, i);
            let origin = Origin::Field {
                parent: found,
                index: i,
            };
            match (want, child.is_null()) {
                (None, true) => {}
                (None, false) => report.record(Violation::NullMismatch {
                    origin,
                    expected_null: true,
                }),
                (Some(_), true) => report.record(Violation::NullMismatch {
                    origin,
                    expected_null: false,
                }),
                (Some(want_id), false) => queue.push((child, origin, Some(*want_id))),
            }
        }
    }

    for (id, at) in &id_at {
        let mut distinct: Vec<Handle> = at.clone();
        distinct.sort();
        distinct.dedup();
        if distinct.len() > 1 {
            report.record(Violation::DuplicatedIdentity {
                id: *id,
                at: distinct,
            });
        }
    }

    // Working out a root path costs a walk of the model, so only the
    // violations that will actually be printed get one. A collector that has
    // lost the whole heap would otherwise spend longer being diagnosed than it
    // spent running.
    let expected_live: BTreeSet<u64> = model.reachable();
    let mut paths_computed = 0;
    for id in &expected_live {
        if id_at.contains_key(id) {
            continue;
        }
        if report.full() {
            report.omitted += 1;
            continue;
        }
        let path = if paths_computed < MAX_PATHS {
            paths_computed += 1;
            path_to(model, *id)
        } else {
            "path not computed".to_string()
        };
        report.record(Violation::Unreachable { id: *id, path });
    }

    report.visited = id_at.len();
    report
}

/// Compare the collector's own accounting against the model.
///
/// Only meaningful immediately after a full collection by a collector that
/// promises to reclaim everything unreachable.
pub fn check_accounting<C: Collector>(gc: &C, model: &Model) -> Option<Violation> {
    let used = gc.used_bytes();
    let reachable = model.reachable_bytes();
    if used as i64 == reachable as i64 {
        return None;
    }
    Some(Violation::Accounting {
        used,
        reachable,
        unreclaimed: used as i64 - reachable as i64,
    })
}
