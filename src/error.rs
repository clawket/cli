// Clawket v3 standardised exit codes.
//
//  2 = Input     — well-formed command but semantically invalid input (bad ID, parse fail)
//  3 = Policy    — daemon rejected the request (409 Conflict, 403 Forbidden, business rule)
//  4 = Daemon    — daemon unreachable or returned an unexpected 5xx
//
// Usage=1 is owned by clap (it exits 1 on bad CLI syntax automatically).
// Software=70 is emitted directly by the panic hook (`std::process::exit(70)` below).

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i32)]
pub enum ExitCode {
    Input = 2,
    Policy = 3,
    Daemon = 4,
}

/// Install a panic hook that prints the panic message to stderr with an ERROR: prefix
/// and exits with code 70 (EX_SOFTWARE) instead of the default Rust abort behaviour.
/// Call once at the top of `main()` (US-CLAWKET-CLI-ERR-004).
pub fn install_panic_hook() {
    std::panic::set_hook(Box::new(|info| {
        let location = info.location().map(|l| format!("{}:{}", l.file(), l.line())).unwrap_or_else(|| "unknown".to_string());
        let msg = if let Some(s) = info.payload().downcast_ref::<&str>() {
            s.to_string()
        } else if let Some(s) = info.payload().downcast_ref::<String>() {
            s.clone()
        } else {
            "unknown panic payload".to_string()
        };
        eprintln!("ERROR: internal error (panic) at {location}: {msg}");
        eprintln!("       Please report this at https://github.com/clawket/clawket/issues");
        std::process::exit(70);
    }));
}

impl ExitCode {
    pub fn exit(self) -> ! {
        std::process::exit(self as i32)
    }
}

/// Classify an anyhow error by inspecting its message for known patterns and
/// exit with the appropriate code. Call at top-level after `main()` returns
/// `Err(e)`.
pub fn exit_from_error(e: &anyhow::Error) -> ! {
    let msg = format!("{e:#}");
    // Daemon connectivity errors (socket missing, connection refused)
    if msg.contains("No such file or directory")
        || msg.contains("Connection refused")
        || msg.contains("failed to connect")
        || msg.contains("daemon")
        || msg.contains("socket")
    {
        ExitCode::Daemon.exit();
    }
    // Policy / business-rule rejections from the daemon (4xx HTTP)
    if msg.contains("409")
        || msg.contains("Conflict")
        || msg.contains("403")
        || msg.contains("Forbidden")
        || msg.contains("already")
        || msg.contains("lease")
        || msg.contains("drift is `major`")
    {
        ExitCode::Policy.exit();
    }
    // Input errors — bad IDs, parse failures, etc.
    ExitCode::Input.exit();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exit_codes_are_distinct() {
        assert_eq!(ExitCode::Input as i32, 2);
        assert_eq!(ExitCode::Policy as i32, 3);
        assert_eq!(ExitCode::Daemon as i32, 4);
    }
}
