use std::env;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::Result;

pub(crate) struct CapturedOutput {
    pub(crate) status: ExitStatus,
    pub(crate) stdout: String,
    pub(crate) stderr: String,
}

pub(crate) fn repo_root() -> Result<PathBuf> {
    let current = env::current_dir()?;
    let relative_root = command_stdout("git", ["rev-parse", "--show-cdup"])?;
    repo_root_from_relative(current, relative_root.trim())
}

fn repo_root_from_relative(mut current: PathBuf, relative_root: &str) -> Result<PathBuf> {
    for component in Path::new(relative_root).components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir if current.pop() => {}
            Component::ParentDir => {
                return Err("repository root is above the filesystem root".into());
            }
            _ => return Err("git returned an invalid relative repository root".into()),
        }
    }
    Ok(current)
}

pub(crate) fn command_stdout<I, S>(program: &str, args: I) -> Result<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let output = Command::new(program).args(args).output()?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("{program} failed with status {}: {stderr}", output.status).into());
    }
    Ok(String::from_utf8(output.stdout)?)
}

pub(crate) fn capture_command<I, S>(program: &str, args: I) -> Result<CapturedOutput>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let output = Command::new(program).args(args).output()?;
    Ok(CapturedOutput {
        status: output.status,
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    })
}

pub(crate) fn run_command<I, S>(program: &str, args: I) -> Result<()>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let status = Command::new(program)
        .args(args)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("{program} failed with status {status}").into())
    }
}

pub(crate) fn ensure_command(command: &str) -> Result<()> {
    if find_on_path(command) {
        Ok(())
    } else {
        Err(format!("{command} is required").into())
    }
}

pub(crate) fn find_on_path(command: &str) -> bool {
    find_on_path_os(command).is_some()
}

pub(crate) fn find_on_path_os(command: &str) -> Option<PathBuf> {
    let names = executable_names(command, env::consts::EXE_SUFFIX);
    let paths = env::var_os("PATH")?;
    env::split_paths(&paths).find_map(|path| {
        names
            .iter()
            .map(|name| path.join(name))
            .find(|path| path.is_file())
    })
}

fn executable_names(command: &str, suffix: &str) -> Vec<OsString> {
    let mut names = vec![OsString::from(command)];
    if !suffix.is_empty() && !command.to_ascii_lowercase().ends_with(suffix) {
        names.push(OsString::from(format!("{command}{suffix}")));
    }
    names
}

pub(crate) fn read_to_string(path: &Path) -> Result<String> {
    fs::read_to_string(path)
        .map_err(|err| format!("failed to read {}: {err}", path.display()).into())
}

pub(crate) fn write_string(path: &Path, contents: &str) -> Result<()> {
    fs::write(path, contents)
        .map_err(|err| format!("failed to write {}: {err}", path.display()).into())
}

pub(crate) fn collect_files_with_extension(
    root: &Path,
    current: &Path,
    extension: &str,
    output: &mut Vec<PathBuf>,
) -> io::Result<()> {
    for entry in fs::read_dir(current)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            collect_files_with_extension(root, &path, extension, output)?;
        } else if path
            .extension()
            .and_then(|value| value.to_str())
            .is_some_and(|value| value == extension)
        {
            let relative = path.strip_prefix(root).map_err(io::Error::other)?;
            output.push(relative.to_path_buf());
        }
    }
    Ok(())
}

pub(crate) fn path_to_slash(path: &Path) -> String {
    path.components()
        .map(|component| component.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

pub(crate) fn quoted_value(line: &str, key: &str) -> Option<String> {
    let prefix = format!("{key} = \"");
    line.strip_prefix(&prefix)
        .and_then(|value| value.strip_suffix('"'))
        .map(ToOwned::to_owned)
}

pub(crate) fn temp_path(label: &str) -> PathBuf {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    env::temp_dir().join(format!("rufin-{label}-{}-{now}", std::process::id()))
}

pub(crate) fn repo_url_from_origin() -> Result<Option<String>> {
    let output =
        command_stdout("git", ["config", "--get", "remote.origin.url"]).unwrap_or_default();
    let mut origin = output.trim().to_owned();
    if origin.is_empty() {
        return Ok(None);
    }
    if let Some(rest) = origin.strip_prefix("git@github.com:") {
        origin = format!("https://github.com/{rest}");
    } else if let Some(rest) = origin.strip_prefix("ssh://git@github.com/") {
        origin = format!("https://github.com/{rest}");
    }
    origin = origin.trim_end_matches(".git").to_owned();
    if origin.starts_with("https://github.com/") {
        Ok(Some(origin))
    } else {
        Ok(None)
    }
}

pub(crate) fn github_repo_from_origin() -> Result<Option<String>> {
    Ok(repo_url_from_origin()?.map(|url| url.trim_start_matches("https://github.com/").to_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repository_root_preserves_the_native_current_path() {
        let root = PathBuf::from("workspace").join("Rufin");
        let current = root.join("crates").join("xtask");

        assert_eq!(repo_root_from_relative(current, "../../").unwrap(), root);
    }

    #[test]
    fn repository_root_rejects_non_parent_paths() {
        let error = repo_root_from_relative(PathBuf::from("workspace/Rufin"), "other/")
            .unwrap_err()
            .to_string();

        assert_eq!(error, "git returned an invalid relative repository root");
    }

    #[test]
    fn windows_executable_lookup_appends_the_suffix() {
        assert_eq!(
            executable_names("gst-launch-1.0", ".exe"),
            ["gst-launch-1.0", "gst-launch-1.0.exe"]
                .map(OsString::from)
                .to_vec()
        );
        assert_eq!(
            executable_names("gpg.exe", ".exe"),
            vec![OsString::from("gpg.exe")]
        );
    }
}
