//! Shared test and benchmark machinery.
//!
//! [`checks`] holds conformance checks: properties any correct collector of a
//! given class must have. A module's test file names the checks that apply to
//! it, so the test suite reads as a specification of what that collector
//! promises. A reference counter, for instance, calls every check except
//! [`checks::collects_cycles`] — and asserts the opposite in its own tests.
//!
//! [`bench`] runs candidate programs and reports time and space.

pub mod bench;
pub mod checks;

pub use bench::{WorkloadSpec, main_bench, run_bench, spec};
