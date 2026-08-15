//! Syncs a fork with its upstream and opens a pull request for the result.
//!
//! The library half holds the logic; the binary is one caller of it, and a
//! long-running server would be another. Keeping them apart is what lets the
//! interesting parts — the boundary in particular — be tested without a forge,
//! a network, or a scheduler.

pub mod boundary;
pub mod config;
pub mod forge;
pub mod git;
pub mod notify;
pub mod runner;
pub mod serve;
pub mod store;
pub mod sync;
