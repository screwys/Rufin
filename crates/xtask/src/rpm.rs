use std::fs;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::Result;
use crate::process::{ensure_command, read_to_string, repo_root, temp_path};
use crate::release::normalize_plain_version;

const REGISTRY_SOURCE: &str = "registry+https://github.com/rust-lang/crates.io-index";

#[derive(Debug, Eq, PartialEq)]
enum RpmSource {
    ReleaseTag(String),
    CandidateRef(String),
}

pub(crate) fn srpm_command(mut args: Vec<String>) -> Result<()> {
    let usage = "Usage: cargo run --locked -p xtask -- generate rpm-srpm [TAG | --candidate-ref REF] --output PATH";
    let mut tag = None;
    let mut candidate_ref = None;
    let mut output = None;

    while !args.is_empty() {
        match args.remove(0).as_str() {
            "-h" | "--help" => {
                eprintln!("{usage}");
                return Ok(());
            }
            "--output" => {
                if args.is_empty() {
                    return Err("--output requires a path".into());
                }
                output = Some(PathBuf::from(args.remove(0)));
            }
            "--candidate-ref" => {
                if args.is_empty() {
                    return Err("--candidate-ref requires a Git ref".into());
                }
                candidate_ref = Some(args.remove(0));
            }
            arg if arg.starts_with('-') => return Err(format!("unknown option: {arg}").into()),
            arg if tag.is_none() => tag = Some(arg.to_owned()),
            arg => return Err(format!("unexpected argument: {arg}").into()),
        }
    }

    let source = select_source(tag, candidate_ref)?;
    let output = output.ok_or("generate rpm-srpm requires --output PATH")?;
    generate_srpm(source, &output)
}

fn select_source(tag: Option<String>, candidate_ref: Option<String>) -> Result<RpmSource> {
    match (tag, candidate_ref) {
        (Some(tag), None) => Ok(RpmSource::ReleaseTag(tag)),
        (None, Some(candidate_ref)) => Ok(RpmSource::CandidateRef(candidate_ref)),
        (Some(_), Some(_)) => Err("pass either TAG or --candidate-ref, not both".into()),
        (None, None) => Err("generate rpm-srpm requires TAG or --candidate-ref".into()),
    }
}

fn generate_srpm(source: RpmSource, output: &Path) -> Result<()> {
    for command in ["cargo", "git", "rpmbuild", "sha256sum", "tar", "xz"] {
        ensure_command(command)?;
    }

    let root = repo_root()?;
    let (source_ref, version, spec) = match source {
        RpmSource::ReleaseTag(tag) => {
            let version = normalize_plain_version(&tag)?.to_owned();
            let tag = format!("v{version}");
            verify_tag(&root, &tag)?;
            let spec_path = root.join("packaging/rpm/rufin.spec");
            if !spec_path.is_file() {
                return Err(format!("missing RPM spec: {}", spec_path.display()).into());
            }
            let spec = read_to_string(&spec_path)?;
            (tag, version, spec)
        }
        RpmSource::CandidateRef(candidate_ref) => {
            verify_ref(&root, &candidate_ref)?;
            let cargo_toml = git_file(&root, &candidate_ref, "Cargo.toml")?;
            let version = normalize_plain_version(workspace_version(&cargo_toml)?)?.to_owned();
            let spec = git_file(&root, &candidate_ref, "packaging/rpm/rufin.spec")?;
            (candidate_ref, version, spec)
        }
    };
    let output = if output.is_absolute() {
        output.to_path_buf()
    } else {
        root.join(output)
    };

    let source_name = format!("Rufin-{version}.tar.xz");
    let vendor_name = format!("Rufin-{version}-vendor.tar.xz");
    refuse_existing_artifacts(&output, [&source_name, &vendor_name, "SHA256SUMS"])?;

    verify_source_inputs(&root, &spec, &source_ref, &version)?;

    let temp = temp_path("rpm-srpm");
    let result = generate_srpm_inner(
        &root,
        &spec,
        &source_ref,
        &version,
        &source_name,
        &vendor_name,
        &output,
        &temp,
    );
    let _ = fs::remove_dir_all(&temp);
    result
}

#[allow(clippy::too_many_arguments)]
fn generate_srpm_inner(
    root: &Path,
    spec: &str,
    source_ref: &str,
    version: &str,
    source_name: &str,
    vendor_name: &str,
    output: &Path,
    temp: &Path,
) -> Result<()> {
    let stage = temp.join("stage");
    let source_tree = temp.join(format!("Rufin-{version}"));
    let raw_source = temp.join(format!("Rufin-{version}.tar"));
    let topdir = temp.join("rpmbuild");
    for directory in [
        &stage,
        &topdir,
        &topdir.join("BUILD"),
        &topdir.join("BUILDROOT"),
    ] {
        fs::create_dir_all(directory)?;
    }

    run(
        Command::new("git")
            .current_dir(root)
            .args(["archive", "--format=tar"])
            .arg(format!("--prefix=Rufin-{version}/"))
            .args(["--output"])
            .arg(&raw_source)
            .arg(source_ref),
        "git archive",
    )?;
    run(
        Command::new("xz")
            .args(["--check=crc64", "--force", "--keep", "-9", "--threads=0"])
            .arg(&raw_source),
        "xz",
    )?;
    let staged_source = stage.join(source_name);
    fs::rename(raw_source.with_extension("tar.xz"), &staged_source)?;
    set_archive_permissions(&staged_source)?;

    let staged_vendor = stage.join(vendor_name);
    run(
        Command::new("tar")
            .args(["-xf"])
            .arg(&raw_source)
            .args(["-C"])
            .arg(temp),
        "tar",
    )?;
    run(
        Command::new("cargo")
            .current_dir(&source_tree)
            .args(["vendor", "--locked", "--versioned-dirs", "vendor"])
            .stdin(Stdio::null()),
        "cargo vendor",
    )?;
    #[cfg(unix)]
    clear_rust_source_executable_bits(&source_tree.join("vendor"))?;

    let timestamp_output = Command::new("git")
        .current_dir(root)
        .args([
            "show",
            "-s",
            "--format=%ct",
            &format!("{source_ref}^{{commit}}"),
        ])
        .output()?;
    if !timestamp_output.status.success() {
        return Err(format!("could not read the commit timestamp for {source_ref}").into());
    }
    let timestamp = String::from_utf8(timestamp_output.stdout)?;
    run(
        Command::new("tar")
            .args([
                "--sort=name",
                &format!("--mtime=@{}", timestamp.trim()),
                "--owner=0",
                "--group=0",
                "--numeric-owner",
                "--format=gnu",
                "-cJf",
            ])
            .arg(&staged_vendor)
            .args(["-C"])
            .arg(&source_tree)
            .arg("vendor"),
        "tar",
    )?;
    set_archive_permissions(&staged_vendor)?;

    let staged_spec = stage.join("rufin.spec");
    fs::write(&staged_spec, spec)?;
    set_archive_permissions(&staged_spec)?;

    run(
        Command::new("rpmbuild")
            .arg("-bs")
            .args(["--define", &format!("_topdir {}", topdir.display())])
            .args(["--define", &format!("_sourcedir {}", stage.display())])
            .args(["--define", &format!("_srcrpmdir {}", stage.display())])
            .arg(&staged_spec),
        "rpmbuild",
    )?;

    let srpm = fs::read_dir(&stage)?
        .filter_map(std::result::Result::ok)
        .map(|entry| entry.path())
        .find(|path| path.to_string_lossy().ends_with(".src.rpm"))
        .ok_or("rpmbuild did not create a source RPM")?;
    let srpm_name = srpm
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or("source RPM name is not valid UTF-8")?
        .to_owned();

    let checksums = Command::new("sha256sum")
        .current_dir(&stage)
        .args([source_name, vendor_name, &srpm_name])
        .output()?;
    if !checksums.status.success() {
        return Err(format!("sha256sum failed with status {}", checksums.status).into());
    }
    fs::write(stage.join("SHA256SUMS"), checksums.stdout)?;
    set_archive_permissions(&stage.join("SHA256SUMS"))?;

    fs::create_dir_all(output)?;
    for name in [source_name, vendor_name, &srpm_name, "SHA256SUMS"] {
        fs::copy(stage.join(name), output.join(name))?;
    }

    eprintln!("Created {}", output.join(&srpm_name).display());
    eprintln!("Sources and checksums are in {}", output.display());
    Ok(())
}

#[cfg(unix)]
fn set_archive_permissions(path: &Path) -> Result<()> {
    fs::set_permissions(path, fs::Permissions::from_mode(0o644))?;
    Ok(())
}

#[cfg(not(unix))]
fn set_archive_permissions(_path: &Path) -> Result<()> {
    Err("RPM source generation is only supported on Unix".into())
}

#[cfg(unix)]
fn clear_rust_source_executable_bits(path: &Path) -> Result<()> {
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            clear_rust_source_executable_bits(&path)?;
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            let mut permissions = entry.metadata()?.permissions();
            let mode = permissions.mode();
            if mode & 0o111 != 0 {
                permissions.set_mode(mode & !0o111);
                fs::set_permissions(path, permissions)?;
            }
        }
    }
    Ok(())
}

fn verify_tag(root: &Path, tag: &str) -> Result<()> {
    run(
        Command::new("git")
            .current_dir(root)
            .args(["tag", "--verify", tag]),
        "git tag --verify",
    )?;

    let tag_commit = format!("{tag}^{{commit}}");
    run(
        Command::new("git").current_dir(root).args([
            "merge-base",
            "--is-ancestor",
            &tag_commit,
            "origin/main",
        ]),
        "release tag ancestry",
    )
}

fn verify_ref(root: &Path, source_ref: &str) -> Result<()> {
    run(
        Command::new("git").current_dir(root).args([
            "rev-parse",
            "--verify",
            &format!("{source_ref}^{{commit}}"),
        ]),
        "candidate ref",
    )
}

fn verify_source_inputs(root: &Path, spec: &str, source_ref: &str, version: &str) -> Result<()> {
    let spec_version = spec_version(spec)?;
    if spec_version != version {
        return Err(format!(
            "RPM spec version is {spec_version}, but {source_ref} contains version {version}"
        )
        .into());
    }
    verify_spec_linux_install(spec)?;

    let cargo_toml = git_file(root, source_ref, "Cargo.toml")?;
    let cargo_version = workspace_version(&cargo_toml)?;
    if cargo_version != version {
        return Err(format!(
            "{source_ref} Cargo.toml version is {cargo_version}, expected {version}"
        )
        .into());
    }

    let metainfo = git_file(
        root,
        source_ref,
        "data/io.github.screwys.Rufin.metainfo.xml",
    )?;
    let metainfo_version = latest_metainfo_version(&metainfo)?;
    if metainfo_version != version {
        return Err(format!(
            "{source_ref} metainfo release is {metainfo_version}, expected {version}"
        )
        .into());
    }

    verify_lock_sources(&git_file(root, source_ref, "Cargo.lock")?)?;
    Ok(())
}

fn verify_spec_linux_install(spec: &str) -> Result<()> {
    for marker in [
        "%cmake -G Ninja",
        "-DRUFIN_CARGO_PROFILE=rpm",
        "%cmake_build --target rufin",
        "%cmake_install",
    ] {
        if !spec.contains(marker) {
            return Err(format!("RPM spec does not use the CMake build: {marker}").into());
        }
    }
    Ok(())
}

fn git_file(root: &Path, tag: &str, path: &str) -> Result<String> {
    let output = Command::new("git")
        .current_dir(root)
        .args(["show", &format!("{tag}:{path}")])
        .output()?;
    if !output.status.success() {
        return Err(format!("could not read {path} from {tag}").into());
    }
    Ok(String::from_utf8(output.stdout)?)
}

fn spec_version(spec: &str) -> Result<&str> {
    spec.lines()
        .find_map(|line| line.strip_prefix("Version:").map(str::trim))
        .filter(|version| !version.is_empty())
        .ok_or_else(|| "RPM spec has no Version field".into())
}

fn workspace_version(cargo_toml: &str) -> Result<&str> {
    let mut in_workspace_package = false;
    for line in cargo_toml.lines() {
        if line == "[workspace.package]" {
            in_workspace_package = true;
            continue;
        }
        if in_workspace_package && line.starts_with('[') {
            break;
        }
        if in_workspace_package
            && let Some(version) = line
                .strip_prefix("version = \"")
                .and_then(|value| value.strip_suffix('"'))
        {
            return Ok(version);
        }
    }
    Err("Cargo.toml has no workspace package version".into())
}

fn latest_metainfo_version(metainfo: &str) -> Result<&str> {
    metainfo
        .lines()
        .find_map(|line| {
            line.split_once("<release version=\"")
                .and_then(|(_, rest)| rest.split_once('"'))
                .map(|(version, _)| version)
        })
        .ok_or_else(|| "metainfo has no release version".into())
}

fn verify_lock_sources(lock: &str) -> Result<()> {
    for source in lock.lines().filter_map(|line| {
        line.strip_prefix("source = \"")
            .and_then(|value| value.strip_suffix('"'))
    }) {
        if source != REGISTRY_SOURCE {
            return Err(
                format!("Cargo.lock contains an unsupported remote source: {source}").into(),
            );
        }
    }
    Ok(())
}

fn refuse_existing_artifacts<'a>(
    output: &Path,
    names: impl IntoIterator<Item = &'a str>,
) -> Result<()> {
    for name in names {
        let path = output.join(name);
        if path.exists() {
            return Err(format!("refusing to overwrite {}", path.display()).into());
        }
    }
    if output.is_dir()
        && fs::read_dir(output)?.any(|entry| {
            entry
                .ok()
                .is_some_and(|entry| entry.path().to_string_lossy().ends_with(".src.rpm"))
        })
    {
        return Err(format!("refusing to overwrite source RPM in {}", output.display()).into());
    }
    Ok(())
}

fn run(command: &mut Command, label: &str) -> Result<()> {
    let status = command
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("{label} failed with status {status}").into())
    }
}

#[cfg(test)]
mod tests {
    use super::{RpmSource, select_source, verify_lock_sources, verify_spec_linux_install};

    #[test]
    fn candidate_ref_is_distinct_from_a_signed_release_tag() {
        assert_eq!(
            select_source(None, Some("HEAD".to_owned())).unwrap(),
            RpmSource::CandidateRef("HEAD".to_owned())
        );
        assert!(select_source(Some("v0.9.0".to_owned()), Some("HEAD".to_owned())).is_err());
    }

    #[test]
    fn remote_git_dependencies_are_rejected() {
        let lock = "source = \"git+https://example.com/dependency\"\n";
        assert!(verify_lock_sources(lock).is_err());
    }

    #[test]
    fn rpm_spec_uses_the_cmake_build() {
        let spec = "%cmake -G Ninja -DRUFIN_CARGO_PROFILE=rpm\n\
                    %cmake_build --target rufin\n\
                    %cmake_install\n";
        verify_spec_linux_install(spec).unwrap();
        assert!(verify_spec_linux_install("Source0: Rufin.tar.xz\n").is_err());
    }
}
