use std::env;
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::Result;
use crate::process::{
    command_stdout, ensure_command, path_to_slash, read_to_string, repo_root, run_command,
    temp_path,
};
use crate::release::{
    first_metainfo_release_version, normalize_tag, workspace_version_from_cargo_toml,
};

pub(crate) fn run(mut args: Vec<String>) -> Result<()> {
    if args.is_empty() {
        return Err("missing verify command".into());
    }

    match args.remove(0).as_str() {
        "package-layout" => package_layout(args),
        "release-metadata" => release_metadata(args),
        "release-tag" => release_tag(args),
        command => Err(format!("unknown verify command: {command}").into()),
    }
}

fn package_layout(args: Vec<String>) -> Result<()> {
    if matches!(args.as_slice(), [arg] if arg == "-h" || arg == "--help") {
        eprintln!("Usage: cargo run --locked -p xtask -- verify package-layout ROOT [PREFIX]");
        return Ok(());
    }
    if args.is_empty() || args.len() > 2 {
        return Err("verify package-layout requires ROOT and optional PREFIX".into());
    }

    let root = trim_root(&args[0]);
    let prefix = trim_prefix(args.get(1).map(String::as_str).unwrap_or("/usr"));
    let mut windows_package = false;

    if prefix.is_empty() {
        let unix_bin = package_path(&root, &prefix, "bin/rufin");
        let windows_bin = package_path(&root, &prefix, "bin/rufin.exe");
        if !unix_bin.is_file() && !windows_bin.is_file() {
            return Err(format!("missing executable under {}", args[0]).into());
        }
        if windows_bin.is_file() {
            windows_package = true;
            require_file(&package_path(
                &root,
                &prefix,
                "share/glib-2.0/schemas/gschemas.compiled",
            ))?;
            require_file(&package_path(
                &root,
                &prefix,
                "share/icons/hicolor/icon-theme.cache",
            ))?;
            verify_windows_updater(&root, &prefix, env!("CARGO_PKG_VERSION"))?;
        }
    } else {
        require_file(&package_path(&root, &prefix, "bin/rufin"))?;
    }

    for path in [
        "share/icons/hicolor/512x512/apps/io.github.screwys.Rufin.png",
        "share/icons/hicolor/scalable/apps/io.github.screwys.Rufin.svg",
        "share/icons/hicolor/symbolic/apps/io.github.screwys.Rufin-symbolic.svg",
        "share/licenses/rufin/japanese-readings.LICENSE",
        "share/rufin/japanese-readings.dic",
    ] {
        require_file(&package_path(&root, &prefix, path))?;
    }
    if !windows_package {
        for path in [
            "share/applications/io.github.screwys.Rufin.desktop",
            "share/metainfo/io.github.screwys.Rufin.metainfo.xml",
        ] {
            require_file(&package_path(&root, &prefix, path))?;
        }
    }

    for entry in fs::read_dir(repo_root()?.join("locales"))? {
        let path = entry?.path();
        if path.extension() != Some(OsStr::new("po")) {
            continue;
        }
        let language = path
            .file_stem()
            .and_then(OsStr::to_str)
            .ok_or_else(|| format!("invalid locale filename: {}", path.display()))?;
        require_file(&package_path(
            &root,
            &prefix,
            &format!("share/locale/{language}/LC_MESSAGES/rufin.mo"),
        ))?;
    }

    Ok(())
}

fn verify_windows_updater(root: &str, prefix: &str, version: &str) -> Result<()> {
    let updater = package_path(
        root,
        prefix,
        &format!("updater/{version}/rufin-update-helper.exe"),
    );
    require_file(&updater)?;
    let sentinel = package_path(
        root,
        prefix,
        &format!("updater/{version}/rufin-update-helper.complete"),
    );
    require_file(&sentinel)?;
    let sentinel_contents = fs::read_to_string(&sentinel)?;
    if sentinel_contents != format!("rufin-update-helper:{version}\n") {
        return Err(format!("invalid update helper sentinel: {}", sentinel.display()).into());
    }
    Ok(())
}

fn trim_root(root: &str) -> String {
    let root = root.trim_end_matches('/').to_owned();
    if root == "/" { String::new() } else { root }
}

fn trim_prefix(prefix: &str) -> String {
    let prefix = format!("/{}", prefix.trim_start_matches('/'))
        .trim_end_matches('/')
        .to_owned();
    if prefix == "/" { String::new() } else { prefix }
}

fn package_path(root: &str, prefix: &str, path: &str) -> PathBuf {
    PathBuf::from(format!("{root}{prefix}/{path}"))
}

fn require_file(path: &Path) -> Result<()> {
    if path.is_file() {
        Ok(())
    } else {
        Err(format!("missing file: {}", path.display()).into())
    }
}

fn release_metadata(args: Vec<String>) -> Result<()> {
    if matches!(args.as_slice(), [arg] if arg == "-h" || arg == "--help") {
        eprintln!("Usage: cargo run --locked -p xtask -- verify release-metadata TAG");
        return Ok(());
    }
    if args.len() != 1 {
        return Err("verify release-metadata requires TAG".into());
    }

    let root = repo_root()?;
    verify_release_metadata_at(&root, &args[0])
}

fn verify_release_metadata_at(root: &Path, tag: &str) -> Result<()> {
    let cargo_toml = read_to_string(&root.join("Cargo.toml"))?;
    let metainfo = read_to_string(&root.join("data/io.github.screwys.Rufin.metainfo.xml"))?;
    verify_release_metadata_contents(tag, &cargo_toml, &metainfo)
}

fn verify_release_metadata_contents(tag: &str, cargo_toml: &str, metainfo: &str) -> Result<()> {
    let tag = normalize_tag(tag.trim_start_matches("refs/tags/"))?;
    let expected = tag.trim_start_matches('v');
    let cargo_version = workspace_version_from_cargo_toml(cargo_toml)?;
    let metainfo_version = first_metainfo_release_version(metainfo)?;

    if cargo_version != expected {
        return Err(
            format!("tag {tag} has Cargo version {cargo_version}, expected {expected}").into(),
        );
    }
    if metainfo_version != expected {
        return Err(format!(
            "tag {tag} has MetaInfo release {metainfo_version}, expected {expected}"
        )
        .into());
    }
    Ok(())
}

fn release_tag(args: Vec<String>) -> Result<()> {
    if matches!(args.as_slice(), [arg] if arg == "-h" || arg == "--help") {
        eprintln!("Usage: cargo run --locked -p xtask -- verify release-tag TAG");
        return Ok(());
    }
    if args.len() != 1 {
        return Err("verify release-tag requires TAG".into());
    }

    let tag = normalize_tag(args[0].trim_start_matches("refs/tags/"))?;
    let root = repo_root()?;
    env::set_current_dir(&root)?;

    let shallow = command_stdout("git", ["rev-parse", "--is-shallow-repository"])?;
    if shallow.trim() == "true" {
        run_command("git", ["fetch", "--force", "--unshallow", "origin"])?;
    }

    run_command(
        "git",
        [
            "fetch",
            "--force",
            "origin",
            "refs/heads/main:refs/remotes/origin/main",
            &format!("refs/tags/{tag}:refs/tags/{tag}"),
        ],
    )?;

    let tag_ref = format!("refs/tags/{tag}");
    let tag_type = command_stdout("git", ["cat-file", "-t", &tag_ref])?;
    if tag_type.trim() != "tag" {
        return Err("release ref must be an annotated signed tag".into());
    }

    let tag_contents = command_stdout("git", ["cat-file", "tag", &tag_ref])?;
    if !tag_contents
        .lines()
        .any(|line| line == "-----BEGIN PGP SIGNATURE-----")
    {
        return Err("release ref must contain a PGP signature".into());
    }

    let release_key = root.join(".github/release-gpg.pub");
    verify_tag_with_release_key(&tag_ref, &root, &release_key)?;

    let tag_target = command_stdout("git", ["rev-list", "-n1", &tag_ref])?;
    let head_commit = command_stdout("git", ["rev-parse", "HEAD"])?;
    if tag_target.trim() != head_commit.trim() {
        return Err("checked-out commit does not match release tag target".into());
    }

    let cargo_path = format!("{tag_ref}:Cargo.toml");
    let metainfo_path = format!("{tag_ref}:data/io.github.screwys.Rufin.metainfo.xml");
    let cargo_toml = command_stdout("git", ["show", cargo_path.as_str()])?;
    let metainfo = command_stdout("git", ["show", metainfo_path.as_str()])?;
    verify_release_metadata_contents(&tag, &cargo_toml, &metainfo)?;

    run_command(
        "git",
        [
            "merge-base",
            "--is-ancestor",
            tag_target.trim(),
            "origin/main",
        ],
    )
}

fn verify_tag_with_release_key(tag_ref: &str, root: &Path, release_key: &Path) -> Result<()> {
    if !release_key.is_file() {
        return Err(format!("release GPG key is missing: {}", release_key.display()).into());
    }
    ensure_command("gpg")?;
    let unique_name = temp_path("gnupg")
        .file_name()
        .ok_or("temporary GPG home has no directory name")?
        .to_owned();
    let (gnupg_home, gnupg_home_env) = isolated_gpg_home(root, &unique_name)?;
    fs::create_dir_all(&gnupg_home)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&gnupg_home, fs::Permissions::from_mode(0o700))?;
    }

    let release_key_relative = release_key
        .strip_prefix(root)
        .map_err(|_| "release GPG key must be inside the repository")?;
    let import_output = Command::new("gpg")
        .args(["--batch", "--import"])
        .arg(path_to_slash(release_key_relative))
        .current_dir(root)
        .env("GNUPGHOME", &gnupg_home_env)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()?;
    if !import_output.status.success() {
        let _ = fs::remove_dir_all(&gnupg_home);
        let stderr = String::from_utf8_lossy(&import_output.stderr);
        return Err(format!("failed to import release GPG key: {}", stderr.trim()).into());
    }

    let verify_output = Command::new("git")
        .args(["verify-tag", tag_ref])
        .current_dir(root)
        .env("GNUPGHOME", &gnupg_home_env)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()?;
    let _ = fs::remove_dir_all(&gnupg_home);
    if verify_output.status.success() {
        Ok(())
    } else {
        let stderr = String::from_utf8_lossy(&verify_output.stderr);
        Err(format!(
            "release tag signature did not verify with .github/release-gpg.pub: {}",
            stderr.trim()
        )
        .into())
    }
}

fn isolated_gpg_home(root: &Path, unique_name: &OsStr) -> Result<(PathBuf, String)> {
    let name = Path::new(unique_name);
    if name.components().count() != 1 {
        return Err("temporary GPG home must be a single directory name".into());
    }
    let environment_path = format!(".local/{}", name.to_string_lossy());
    Ok((root.join(".local").join(name), environment_path))
}

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;
    use std::path::Path;

    use super::{isolated_gpg_home, verify_release_metadata_contents};

    const CARGO_TOML: &str = "[workspace.package]\nversion = \"1.2.3\"\n";
    const METAINFO: &str = "<component>\n  <releases>\n    <release version=\"1.2.3\" date=\"2026-08-01\" />\n  </releases>\n</component>\n";

    #[test]
    fn isolated_gpg_home_uses_a_short_portable_environment_path() {
        let root = Path::new("/checkout/with/a/long/native/path");
        let (home, environment_path) =
            isolated_gpg_home(root, OsStr::new("rufin-gnupg-test")).unwrap();

        assert_eq!(home, root.join(".local").join("rufin-gnupg-test"));
        assert_eq!(environment_path, ".local/rufin-gnupg-test");
    }

    #[test]
    fn release_metadata_matches_tag_version() {
        verify_release_metadata_contents("refs/tags/v1.2.3", CARGO_TOML, METAINFO).unwrap();
    }

    #[test]
    fn release_metadata_rejects_a_stale_top_release() {
        let error = verify_release_metadata_contents(
            "v1.2.4",
            "[workspace.package]\nversion = \"1.2.4\"\n",
            METAINFO,
        )
        .unwrap_err();
        assert!(error.to_string().contains("MetaInfo release 1.2.3"));
    }
}
