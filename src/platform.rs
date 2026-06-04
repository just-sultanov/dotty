/// Known platform identifiers.
pub const KNOWN_PLATFORMS: &[&str] = &["macos", "linux", "freebsd", "windows"];

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
    if cfg!(target_os = "windows") {
        return Some("windows".into());
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_known_platforms_contains_expected() {
        assert!(KNOWN_PLATFORMS.contains(&"macos"));
        assert!(KNOWN_PLATFORMS.contains(&"linux"));
        assert!(KNOWN_PLATFORMS.contains(&"freebsd"));
        assert!(KNOWN_PLATFORMS.contains(&"windows"));
        assert_eq!(KNOWN_PLATFORMS.len(), 4);
    }

    #[test]
    fn test_detect_platform_returns_known() {
        let platform = detect_platform();
        assert!(platform.is_some());
        let p = platform.unwrap();
        assert!(KNOWN_PLATFORMS.contains(&p.as_str()));
    }

    #[test]
    fn test_detect_platform_current_platform() {
        let platform = detect_platform().unwrap();
        #[cfg(target_os = "macos")]
        assert_eq!(platform, "macos");
        #[cfg(target_os = "linux")]
        assert_eq!(platform, "linux");
        #[cfg(target_os = "freebsd")]
        assert_eq!(platform, "freebsd");
        #[cfg(target_os = "windows")]
        assert_eq!(platform, "windows");
    }
}
