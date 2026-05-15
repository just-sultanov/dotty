use std::process::Command;

/// Known platform identifiers.
pub const KNOWN_PLATFORMS: &[&str] = &["macos", "linux", "freebsd"];

/// Detect the current platform via `uname -s`.
///
/// Returns `Some("macos")`, `Some("linux")`, `Some("freebsd")`, or `None`
/// for unknown platforms.
pub fn detect_platform() -> Option<String> {
    let output = Command::new("uname").arg("-s").output().ok()?;
    let sysname = String::from_utf8(output.stdout).ok()?.trim().to_string();

    match sysname.as_str() {
        "Darwin" => Some("macos".into()),
        "Linux" => Some("linux".into()),
        "FreeBSD" => Some("freebsd".into()),
        _ => None,
    }
}
