// clawket migrate — copy legacy lattice data to clawket XDG paths.
//
// Runs automatically inside the daemon on first boot (see daemon/paths.rs),
// but this CLI entry point lets users preview, redirect, or force the move.

use anyhow::{Context, Result, bail};
use std::path::PathBuf;

use crate::paths;

pub fn run(dry_run: bool, from: Option<String>, force: bool) -> Result<()> {
    let legacy_dir = from.map(PathBuf::from).unwrap_or_else(default_legacy_dir);
    let legacy_db = legacy_dir.join("db.sqlite");
    let target_db = paths::data_dir().join("db.sqlite");

    println!("clawket migrate");
    println!("  from : {}", legacy_db.display());
    println!("  to   : {}", target_db.display());
    println!("  mode : dry_run={dry_run}, force={force}");
    println!();

    if !legacy_db.exists() {
        println!("legacy DB not found — nothing to do");
        return Ok(());
    }

    if target_db.exists() && !force {
        bail!(
            "target already exists: {} (pass --force to overwrite)",
            target_db.display()
        );
    }

    if dry_run {
        let size = std::fs::metadata(&legacy_db).map(|m| m.len()).unwrap_or(0);
        println!("DRY RUN: would copy {} bytes", size);
        return Ok(());
    }

    if let Some(parent) = target_db.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    std::fs::copy(&legacy_db, &target_db)
        .with_context(|| format!("copy {} -> {}", legacy_db.display(), target_db.display()))?;

    let marker = legacy_db.with_extension("sqlite.migrated-to-clawket");
    std::fs::rename(&legacy_db, &marker)
        .with_context(|| format!("rename {} -> {}", legacy_db.display(), marker.display()))?;

    println!("migrated OK");
    println!("  legacy renamed to: {}", marker.display());
    Ok(())
}

fn default_legacy_dir() -> PathBuf {
    let base = std::env::var("XDG_DATA_HOME")
        .map(PathBuf::from)
        .ok()
        .or_else(|| {
            std::env::var("HOME")
                .ok()
                .map(|h| PathBuf::from(h).join(".local/share"))
        })
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("lattice")
}
