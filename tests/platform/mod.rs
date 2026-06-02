//! Platform-specific test modules.
//!
//! This module organizes tests that are specific to different operating systems.
//! Each platform module is conditionally compiled using `#[cfg(...)]` attributes.

#[cfg(windows)]
mod windows_tests;

#[cfg(target_os = "macos")]
mod macos_tests;

#[cfg(target_os = "linux")]
mod linux_tests;

#[cfg(unix)]
mod unix_tests;
