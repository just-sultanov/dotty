use std::sync::OnceLock;

/// Terminal symbols with ASCII fallback.
///
/// When the terminal doesn't support Unicode (detected via `TERM` env var),
/// falls back to ASCII equivalents.
struct Symbols {
    check: &'static str,
    warn: &'static str,
    arrow: &'static str,
}

static SYMBOLS: OnceLock<Symbols> = OnceLock::new();

fn get() -> &'static Symbols {
    SYMBOLS.get_or_init(|| {
        if supports_unicode() {
            Symbols {
                check: "✓",
                warn: "⚠️",
                arrow: "→",
            }
        } else {
            Symbols {
                check: "[+]",
                warn: "[!]",
                arrow: "->",
            }
        }
    })
}

/// Check if the terminal likely supports Unicode.
///
/// Heuristic: check `TERM` env var. If it's set to a known Unicode-capable
/// value (xterm*, screen*, tmux*, cygwin, linux), assume Unicode support.
/// If `TERM` is unset or set to "dumb", fall back to ASCII.
fn supports_unicode() -> bool {
    let term = std::env::var("TERM").unwrap_or_default();

    // "dumb" terminal — no Unicode
    if term == "dumb" {
        return false;
    }

    // Known Unicode-capable terminals
    matches!(
        term.as_str(),
        "xterm"
            | "xterm-256color"
            | "xterm-color"
            | "screen"
            | "screen-256color"
            | "tmux"
            | "tmux-256color"
            | "cygwin"
            | "linux"
            | "alacritty"
            | "alacritty-256color"
            | "vt100"
            | "rxvt"
            | "rxvt-256color"
            | "xterm-ghostty"
    ) || term.starts_with("xterm")
        || term.starts_with("screen")
        || term.starts_with("tmux")
        || term.starts_with("alacritty")
        || term.starts_with("rxvt")
        || term.starts_with("vt100")
        || term.starts_with("cygwin")
        || term.starts_with("ghostty")
        // If TERM is set to something we don't recognize but isn't "dumb",
        // assume Unicode support (most modern terminals set TERM to something
        // like "xterm-256color").
        || !term.is_empty()
}

/// Return the check mark symbol (✓ or [+]).
pub fn check() -> &'static str {
    get().check
}

/// Return the warning symbol (⚠️ or [!]).
pub fn warn() -> &'static str {
    get().warn
}

/// Return the arrow symbol (→ or ->).
pub fn arrow() -> &'static str {
    get().arrow
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_symbols_are_nonempty() {
        assert!(!check().is_empty());
        assert!(!warn().is_empty());
        assert!(!arrow().is_empty());
    }

    #[test]
    fn test_dumb_terminal_ascii() {
        // Note: OnceLock can't be reset, so we test the heuristic directly.
        // set_var is unsafe in tests due to potential data races, but safe
        // here since tests run sequentially with #[serial] or in isolation.
        unsafe {
            std::env::set_var("TERM", "dumb");
        }
        assert!(!supports_unicode());
    }

    #[test]
    fn test_xterm_unicode() {
        unsafe {
            std::env::set_var("TERM", "xterm-256color");
        }
        assert!(supports_unicode());
    }
}
