use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::fs;
use std::path::Path;
use std::process::Command;

use crate::process::{read_to_string, repo_root, temp_path, write_string};
use crate::{Result, parse_check_flag};

const PACKAGE_START_MARKER: &str = "# Generated Linux package dependencies start.";
const PACKAGE_END_MARKER: &str = "# Generated Linux package dependencies end.";
const DEVELOPMENT_START_MARKER: &str = "# Generated Linux development dependencies start.";
const DEVELOPMENT_END_MARKER: &str = "# Generated Linux development dependencies end.";

const CARGO_NATIVE_CRATES: &[&str] = &[
    "adw",
    "glib",
    "gstreamer",
    "gstreamer-app",
    "gstreamer-audio",
    "gstreamer-pbutils",
    "gtk",
];
const CARGO_NATIVE_CRATE_CANDIDATES: &[&str] = &[
    "adw",
    "gdk-pixbuf",
    "glib",
    "gstreamer",
    "gstreamer-app",
    "gstreamer-audio",
    "gstreamer-pbutils",
    "gtk",
];
const NIX_PACKAGES: &[&str] = &["glib", "gtk4", "libadwaita", "glib-networking"];
const NIX_GSTREAMER_PACKAGES: &[&str] = &[
    "gstreamer",
    "gst-plugins-base",
    "gst-plugins-good",
    "gst-plugins-bad",
    "gst-plugins-ugly",
    "gst-libav",
];
const ARCH_DEPENDENCIES: &[&str] = &[
    "libgcc_s.so",
    "glib2",
    "glibc",
    "gst-libav",
    "gst-plugins-bad",
    "gst-plugins-base",
    "gst-plugins-base-libs",
    "gst-plugins-good",
    "gst-plugins-ugly",
    "gstreamer",
    "gtk4",
    "hicolor-icon-theme",
    "libadwaita",
];
const ARCH_GIT_BUILD_DEPENDENCIES: &[&str] =
    &["cargo", "cmake", "gettext", "git", "ninja", "pkgconf"];

pub(crate) fn command(args: Vec<String>) -> Result<()> {
    let Some(check) = parse_check_flag(
        args,
        "Usage: cargo run --locked -p xtask -- generate linux-packaging [--check]",
    )?
    else {
        return Ok(());
    };
    generate(check)
}

fn generate(check: bool) -> Result<()> {
    let root = repo_root()?;
    verify_cargo_native_crates(&root)?;

    project_generated_block(
        &root.join("flake.nix"),
        &render_flake_package(),
        check,
        PACKAGE_START_MARKER,
        PACKAGE_END_MARKER,
    )?;
    project_generated_block(
        &root.join("flake.nix"),
        &render_flake_development(),
        check,
        DEVELOPMENT_START_MARKER,
        DEVELOPMENT_END_MARKER,
    )?;
    project_generated_block(
        &root.join("packaging/aur/rufin-bin/PKGBUILD.template"),
        &render_pkgbuild(&[]),
        check,
        PACKAGE_START_MARKER,
        PACKAGE_END_MARKER,
    )?;
    project_generated_block(
        &root.join("packaging/aur/rufin-git/PKGBUILD"),
        &render_pkgbuild(ARCH_GIT_BUILD_DEPENDENCIES),
        check,
        PACKAGE_START_MARKER,
        PACKAGE_END_MARKER,
    )?;
    project_srcinfo(&root, check)
}

fn verify_cargo_native_crates(root: &Path) -> Result<()> {
    let mut manifests = fs::read_dir(root.join("crates"))?
        .filter_map(std::result::Result::ok)
        .map(|entry| entry.path().join("Cargo.toml"))
        .filter(|path| path.is_file())
        .collect::<Vec<_>>();
    manifests.sort();

    let mut actual = BTreeSet::new();
    for manifest in manifests {
        actual.extend(cargo_native_dependencies(&read_to_string(&manifest)?));
    }
    let expected = CARGO_NATIVE_CRATES
        .iter()
        .map(|name| (*name).to_owned())
        .collect::<BTreeSet<_>>();

    if actual == expected {
        Ok(())
    } else {
        Err(format!(
            "Cargo native crates differ from Linux packaging metadata; expected {}, found {}",
            display_set(&expected),
            display_set(&actual)
        )
        .into())
    }
}

fn cargo_native_dependencies(manifest: &str) -> BTreeSet<String> {
    manifest
        .lines()
        .filter_map(|line| line.split_once('=').map(|(name, _)| name.trim()))
        .map(|name| name.strip_suffix(".workspace").unwrap_or(name))
        .filter(|name| CARGO_NATIVE_CRATE_CANDIDATES.contains(name))
        .map(ToOwned::to_owned)
        .collect()
}

fn display_set(values: &BTreeSet<String>) -> String {
    values.iter().cloned().collect::<Vec<_>>().join(", ")
}

fn render_flake_package() -> String {
    let mut output = String::new();
    output.push_str("            buildInputs =\n");
    output.push_str("              with pkgs;\n");
    output.push_str("              [\n");
    for package in NIX_PACKAGES {
        let _ = writeln!(output, "                {package}");
    }
    output.push_str("              ]\n");
    output.push_str("              ++ (with gst_all_1; [\n");
    for package in NIX_GSTREAMER_PACKAGES {
        let _ = writeln!(output, "                {package}");
    }
    output.push_str("              ]);\n");
    output
}

fn render_flake_development() -> String {
    let mut output = String::new();
    output.push_str("              ++ (with pkgs; [\n");
    for package in NIX_PACKAGES {
        let _ = writeln!(output, "                {package}");
    }
    output.push_str("              ])\n");
    output.push_str("              ++ (with pkgs.gst_all_1; [\n");
    for package in NIX_GSTREAMER_PACKAGES {
        let _ = writeln!(output, "                {package}");
    }
    output.push_str("              ]);\n");
    output
}

fn render_pkgbuild(build_dependencies: &[&str]) -> String {
    let mut output = String::new();
    output.push_str("depends=(\n");
    for dependency in ARCH_DEPENDENCIES {
        let _ = writeln!(output, "  '{dependency}'");
    }
    output.push_str(")\n");
    if build_dependencies.is_empty() {
        return output;
    }

    output.push_str("makedepends=(\n");
    for dependency in build_dependencies {
        let _ = writeln!(output, "  '{dependency}'");
    }
    output.push_str(")\n");
    output
}

fn project_generated_block(
    path: &Path,
    body: &str,
    check: bool,
    start_marker: &str,
    end_marker: &str,
) -> Result<()> {
    let current = read_to_string(path)?;
    let generated =
        replace_generated_block(&current, start_marker, end_marker, body).map_err(|error| {
            format!(
                "{} has an invalid generated dependency block: {error}",
                path.display()
            )
        })?;
    if current == generated {
        return Ok(());
    }
    if check {
        return Err(format!("{} is stale; run just deps", path.display()).into());
    }
    write_string(path, &generated)
}

fn replace_generated_block(
    input: &str,
    start_marker: &str,
    end_marker: &str,
    body: &str,
) -> Result<String> {
    let mut starts = Vec::new();
    let mut ends = Vec::new();
    let mut offset = 0;

    for line in input.split_inclusive('\n') {
        let line_start = offset;
        offset += line.len();
        if line.contains(start_marker) {
            starts.push((line_start, offset));
        }
        if line.contains(end_marker) {
            ends.push((line_start, offset));
        }
    }

    let (start_line, after_start) = match starts.as_slice() {
        [marker] => *marker,
        _ => return Err("expected exactly one start marker".into()),
    };
    let (end_line, _) = match ends.as_slice() {
        [marker] => *marker,
        _ => return Err("expected exactly one end marker".into()),
    };
    if start_line >= end_line || after_start > end_line {
        return Err("end marker must follow the start marker".into());
    }

    let mut generated = String::with_capacity(input.len() + body.len());
    generated.push_str(&input[..after_start]);
    generated.push_str(body);
    generated.push_str(&input[end_line..]);
    Ok(generated)
}

fn project_srcinfo(root: &Path, check: bool) -> Result<()> {
    let package_dir = root.join("packaging/aur/rufin-git");
    let srcinfo = package_dir.join(".SRCINFO");
    let generated = generate_srcinfo(&package_dir)?;
    let current = fs::read(&srcinfo)
        .map_err(|error| format!("failed to read {}: {error}", srcinfo.display()))?;
    if current == generated {
        return Ok(());
    }
    if check {
        return Err(format!("{} is stale; run just deps", srcinfo.display()).into());
    }
    fs::write(&srcinfo, generated)
        .map_err(|error| format!("failed to write {}: {error}", srcinfo.display()).into())
}

fn generate_srcinfo(package_dir: &Path) -> Result<Vec<u8>> {
    let temp_dir = temp_path("aur-srcinfo");
    fs::create_dir_all(&temp_dir)?;
    let result = (|| {
        let mut command = Command::new("makepkg");
        if fs::File::open("/etc/makepkg.conf").is_err() {
            let config = temp_dir.join("makepkg.conf");
            fs::write(
                &config,
                "CARCH=\"x86_64\"\nCHOST=\"x86_64-pc-linux-gnu\"\nPKGEXT='.pkg.tar.zst'\nSRCEXT='.src.tar.gz'\n",
            )?;
            command.arg("--config").arg(config);
        }
        let output = command
            .arg("--printsrcinfo")
            .current_dir(package_dir)
            .output()
            .map_err(|error| format!("failed to run makepkg: {error}"))?;
        if !output.status.success() {
            return Err(format!(
                "makepkg --printsrcinfo failed with status {}: {}",
                output.status,
                String::from_utf8_lossy(&output.stderr)
            )
            .into());
        }
        Ok(output.stdout)
    })();
    let _ = fs::remove_dir_all(temp_dir);
    result
}
