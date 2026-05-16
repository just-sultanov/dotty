/// Known platform identifiers.
pub const KNOWN_PLATFORMS: &[&str] = &["macos", "linux", "freebsd"];

/// Detect the current platform at compile time via `cfg!(target_os = ...)`.
///
/// Returns `Some("macos")`, `Some("linux")`, `Some("freebsd")`, or `None`
/// for unknown platforms.
pub fn detect_platform() -> Option<String> {
    if cfg!(target_os = "macos") {
        return Some("macos".into());
    }
    if cfg!(target_os = "linux") {
        return Some("linux".into());
    }
    if cfg!(target_os = "freebsd") {
        return Some("freebsd".into());
    }
    None
}
