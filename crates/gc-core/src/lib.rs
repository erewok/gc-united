//! Shared substrate for the GC United exercises.
//!
//! Nothing in this crate implements garbage collection. It provides the raw
//! heap, the root set, the mutator API and an independent verifier; each
//! module crate supplies a collector.

pub mod collector;
pub mod heap;
pub mod model;
pub mod mutator;
pub mod roots;
pub mod space;
pub mod stats;
pub mod verify;

pub mod prelude {
    pub use crate::collector::{Collector, GcError};
    pub use crate::heap::{
        ALIGN, BLACK, FLAG_BUFFERED, FLAG_FREE, FLAG_MARK, FLAG_PINNED, GREY, HEADER_SIZE, Handle,
        Heap, NULL, WHITE, align_up,
    };
    pub use crate::model::Model;
    pub use crate::mutator::Mutator;
    pub use crate::roots::{RootSlot, Roots};
    pub use crate::space::FreeListSpace;
    pub use crate::stats::GcStats;
    pub use crate::verify::{Report, Violation};
}
