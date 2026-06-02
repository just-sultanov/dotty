//! Platform-specific integration tests.
//!
//! This test file includes platform-specific tests that are conditionally
//! compiled based on the target platform.

mod common;
mod platform;

// Re-export TestEnv for use in platform tests
pub use common::TestEnv;
