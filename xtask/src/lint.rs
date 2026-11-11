use std::path::Path;

use crate::Result;
use crate::process::{command_stdout, repo_root, run_command};

const USAGE: &str = "Usage: cargo run --locked -p xtask -- lint BASE";

/// Checks the commits since BASE before they are published: whitespace, dependency policy,
/// formatting and Clippy for the crates they touch. `just check` is the full gate.
pub(crate) fn run(args: Vec<String>) -> Result<()> {
    let [base] = args.as_slice() else {
        return Err(USAGE.into());
    };
    if base == "-h" || base == "--help" {
        eprintln!("{USAGE}");
        return Ok(());
    }
    let root = repo_root()?;
    std::env::set_current_dir(&root)?;
    let range = format!("{base}...HEAD");

    run_command("git", ["diff", "--check", range.as_str()])?;
    run_command(
        "cargo",
        ["deny", "--locked", "check", "-D", "unmatched-skip"],
    )?;
    run_command("cargo", ["fmt", "--all", "--", "--check"])?;

    let changed = command_stdout(
        "git",
        ["diff", "--name-only", "--no-renames", range.as_str()],
    )?;
    let packages = clippy_packages(&root, changed.lines().collect())?;
    if packages.is_empty() {
        return Ok(());
    }
    let mut clippy = vec!["clippy", "--all-targets", "--locked"];
    clippy.extend(packages.iter().map(String::as_str));
    clippy.extend(["--", "-D", "warnings"]);
    run_command("cargo", clippy)
}

/// Clippy arguments for the changed paths: the packages that contain them, or the whole
/// workspace when its manifest, lockfile, toolchain or Cargo configuration changed.
fn clippy_packages(root: &Path, changed: Vec<&str>) -> Result<Vec<String>> {
    let workspace_input = |path: &&str| {
        matches!(
            *path,
            "Cargo.toml" | "Cargo.lock" | "rust-toolchain" | "rust-toolchain.toml"
        ) || path.starts_with(".cargo/")
    };
    if changed.iter().any(workspace_input) {
        return Ok(vec!["--workspace".to_owned()]);
    }

    let metadata: serde_json::Value = serde_json::from_str(&command_stdout(
        "cargo",
        ["metadata", "--locked", "--no-deps", "--format-version", "1"],
    )?)?;
    let mut members: Vec<(String, String)> = metadata["packages"]
        .as_array()
        .ok_or("cargo metadata has no packages")?
        .iter()
        .filter_map(|package| {
            let name = package["name"].as_str()?;
            let manifest = Path::new(package["manifest_path"].as_str()?);
            let directory = manifest.parent()?.strip_prefix(root).ok()?;
            Some((crate::process::path_to_slash(directory), name.to_owned()))
        })
        .collect();
    // The deepest directory owns a path.
    members.sort_by_key(|(directory, _)| std::cmp::Reverse(directory.len()));

    let mut packages = Vec::new();
    for path in changed {
        let owner = members.iter().find(|(directory, _)| {
            directory.is_empty() || path.starts_with(&format!("{directory}/"))
        });
        if let Some((_, name)) = owner
            && !packages.contains(&format!("--package={name}"))
        {
            packages.push(format!("--package={name}"));
        }
    }
    Ok(packages)
}
