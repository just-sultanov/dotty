use std::sync::Mutex;

/// Terminal symbols with ASCII fallback.
///
/// When the terminal doesn't support Unicode (detected via `TERM` env var),
/// falls back to ASCII equivalents.
#[derive(Copy, Clone)]
struct Symbols {
    check: &'static str,
    warn: &'static str,
}

static SYMBOLS: Mutex<Option<Symbols>> = Mutex::new(None);

fn get() -> Symbols {
    let mut lock = match SYMBOLS.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    *lock.get_or_insert_with(|| {
        if supports_unicode() {
            Symbols {
                check: "✓",
                warn: "⚠️",
            }
        } else {
            Symbols {
                check: "[+]",
                warn: "[!]",
            }
        }
    })
}

/// Reset cached symbols (test-only).
///
/// Allows tests to reinitialize symbols with a different terminal
/// environment without requiring serial execution.
#[cfg(test)]
pub fn reset() {
    let mut lock = match SYMBOLS.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    *lock = None;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_symbols_are_nonempty() {
        reset();
        assert!(!check().is_empty());
        assert!(!warn().is_empty());
    }

    #[test]
    fn test_dumb_terminal_ascii_symbols() {
        temp_env::with_var("TERM", Some("dumb"), || {
            reset();
            assert_eq!(check(), "[+]");
            assert_eq!(warn(), "[!]");
        });
    }

    #[test]
    fn test_dumb_terminal_ascii_heuristic() {
        temp_env::with_var("TERM", Some("dumb"), || {
            assert!(!supports_unicode());
        });
    }

    #[test]
    fn test_xterm_unicode() {
        temp_env::with_var("TERM", Some("xterm-256color"), || {
            assert!(supports_unicode());
        });
    }
}
