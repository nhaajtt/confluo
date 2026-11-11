//! The Confluo download engine.

pub mod disk;
pub mod download;
pub mod error;
pub mod hash;
pub mod http;
pub mod journal;
pub mod limit;
pub mod manager;
pub mod name;
pub mod probe;
pub mod ranges;
pub mod routes;
pub mod sched;
pub mod secret;
pub mod tuner;

pub use download::{Command, Config, Outcome, Phase, Route, Snapshot, Spec, Verdict, download};
pub use error::{Error, Result};
pub use hash::Checksum;
