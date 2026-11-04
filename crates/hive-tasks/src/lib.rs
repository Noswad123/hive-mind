//! Local fake-only first-due execution. No Google mutation capability exists here.
mod executor;
pub mod policy;
mod storage;
pub use executor::{FakeSession, SourceOptions, initialize, inspect};
