use std::path::PathBuf;

fn home_dir() -> PathBuf {
    PathBuf::from(std::env::var("HOME").expect("HOME not set"))
}

fn xdg(env_name: &str, fallback: &str) -> PathBuf {
    if let Ok(v) = std::env::var(env_name) {
        PathBuf::from(v)
    } else {
        home_dir().join(fallback)
    }
}

fn resolve(override_env: &str, xdg_var: &str, xdg_fallback: &str) -> PathBuf {
    if let Ok(v) = std::env::var(override_env) {
        return PathBuf::from(v);
    }
    xdg(xdg_var, xdg_fallback).join("clawket")
}

pub fn cache_dir() -> PathBuf {
    resolve("CLAWKET_CACHE_DIR", "XDG_CACHE_HOME", ".cache")
}

pub fn socket_path() -> PathBuf {
    if let Ok(v) = std::env::var("CLAWKET_SOCKET") {
        PathBuf::from(v)
    } else {
        cache_dir().join("clawketd.sock")
    }
}

pub fn pid_path() -> PathBuf {
    cache_dir().join("clawketd.pid")
}

pub fn port_path() -> PathBuf {
    cache_dir().join("clawketd.port")
}

/// Ordered candidate paths for the `clawketd` binary, paired with the reason
/// label shown by `clawket doctor`. Single source of truth shared by
/// `daemon::clawketd_cmd` (which just iterates paths) and `doctor` (which
/// reports which slot matched).
///
/// `current_exe()` is canonicalized: on macOS it returns the symlink target
/// of `~/.local/bin/clawket -> <pluginRoot>/bin/clawket` un-resolved, so
/// without canonicalize the plugin-layout candidate would resolve to
/// `~/.local/daemon/bin/clawketd` (nonexistent) and miss every install.
pub fn daemon_bin_candidates() -> Vec<(PathBuf, &'static str)> {
    let bin_dir = std::env::current_exe().ok().and_then(|exe| {
        let exe = exe.canonicalize().unwrap_or(exe);
        exe.parent().map(PathBuf::from)
    });
    let data_home = std::env::var("XDG_DATA_HOME")
        .map(PathBuf::from)
        .ok()
        .or_else(|| {
            std::env::var("HOME")
                .ok()
                .map(|h| PathBuf::from(h).join(".local/share"))
        });
    daemon_bin_candidates_inner(bin_dir.as_deref(), data_home.as_deref())
}

fn daemon_bin_candidates_inner(
    bin_dir: Option<&std::path::Path>,
    data_home: Option<&std::path::Path>,
) -> Vec<(PathBuf, &'static str)> {
    let mut out = Vec::new();
    if let Some(bin_dir) = bin_dir {
        out.push((
            bin_dir
                .join("..")
                .join("daemon")
                .join("bin")
                .join("clawketd"),
            "plugin layout",
        ));
        out.push((bin_dir.join("clawketd"), "sibling"));
    }
    if let Some(base) = data_home {
        out.push((
            base.join("clawket").join("bin").join("clawketd"),
            "XDG install",
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn plugin_layout_resolves_via_pluginroot_bin() {
        let bin = Path::new("/opt/plugin/bin");
        let xdg = Path::new("/home/u/.local/share");
        let cands = daemon_bin_candidates_inner(Some(bin), Some(xdg));

        assert_eq!(cands.len(), 3);
        assert_eq!(cands[0].1, "plugin layout");
        assert_eq!(
            cands[0].0,
            Path::new("/opt/plugin/bin/../daemon/bin/clawketd")
        );
        assert_eq!(cands[1].1, "sibling");
        assert_eq!(cands[1].0, Path::new("/opt/plugin/bin/clawketd"));
        assert_eq!(cands[2].1, "XDG install");
        assert_eq!(
            cands[2].0,
            Path::new("/home/u/.local/share/clawket/bin/clawketd")
        );
    }

    #[test]
    fn xdg_only_when_no_exe_dir() {
        let xdg = Path::new("/home/u/.local/share");
        let cands = daemon_bin_candidates_inner(None, Some(xdg));
        assert_eq!(cands.len(), 1);
        assert_eq!(cands[0].1, "XDG install");
    }

    #[test]
    fn empty_when_nothing_known() {
        assert!(daemon_bin_candidates_inner(None, None).is_empty());
    }
}
