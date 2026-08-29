use std::collections::HashSet;
use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::Result;
use crate::generate;
use crate::process::{
    capture_command, command_stdout, find_on_path, github_repo_from_origin, read_to_string,
    repo_root, repo_url_from_origin, run_command, temp_path, write_string,
};

const FLATHUB_MANIFEST: &str = "packaging/flatpak/io.github.screwys.Rufin.flathub.json";
const RPM_SPEC: &str = "packaging/rpm/rufin.spec";
const LATEST_RELEASE_SUFFIX: &str = " (latest release)";
const DEVELOPMENT_VERSION: &str = "main (development)";

pub(crate) fn run(mut args: Vec<String>) -> Result<()> {
    if args.is_empty() {
        return Err("missing release command".into());
    }

    match args.remove(0).as_str() {
        "prepare" => prepare(args),
        "create-tag" => create_tag(args),
        "update-flathub-manifest" => update_flathub_manifest(args),
        command => Err(format!("unknown release command: {command}").into()),
    }
}

fn prepare(args: Vec<String>) -> Result<()> {
    if matches!(args.as_slice(), [arg] if arg == "-h" || arg == "--help") {
        eprintln!("Usage: cargo run --locked -p xtask -- release prepare VERSION SUMMARY");
        return Ok(());
    }

    if args.len() != 2 {
        eprintln!("Usage: cargo run --locked -p xtask -- release prepare VERSION SUMMARY");
        return Err("release prepare requires VERSION and SUMMARY".into());
    }

    let version = normalize_plain_version(&args[0])?;
    let notes = &args[1];
    if notes.is_empty() {
        return Err("release notes are required".into());
    }

    prepare_version(&version, notes)
}

fn prepare_version(version: &str, notes: &str) -> Result<()> {
    let root = repo_root()?;
    env::set_current_dir(&root)?;
    let release_date = match env::var("RELEASE_DATE") {
        Ok(date) => date,
        Err(_) => command_stdout("date", ["-u", "+%F"])?.trim().to_owned(),
    };

    replace_workspace_version(version)?;
    update_rpm_spec_version(version)?;
    run_command("cargo", ["update", "--workspace", "--offline"])?;
    update_metainfo_release(version, &release_date, notes)?;
    update_issue_template_versions(version)?;

    Ok(())
}

fn update_flathub_manifest(mut args: Vec<String>) -> Result<()> {
    let mut manifest = PathBuf::from(FLATHUB_MANIFEST);

    while !args.is_empty() {
        match args.remove(0).as_str() {
            "--manifest" => {
                if args.is_empty() {
                    return Err("--manifest requires a path".into());
                }
                manifest = PathBuf::from(args.remove(0));
            }
            "-h" | "--help" => {
                eprintln!(
                    "Usage: cargo run --locked -p xtask -- release update-flathub-manifest [--manifest PATH] TAG"
                );
                return Ok(());
            }
            "--" => break,
            arg if arg.starts_with('-') => return Err(format!("unknown option: {arg}").into()),
            arg => {
                args.insert(0, arg.to_owned());
                break;
            }
        }
    }

    if args.len() != 1 {
        return Err("release update-flathub-manifest requires TAG".into());
    }

    update_flathub_manifest_path(&manifest, &args[0])
}

fn update_flathub_manifest_path(manifest: &Path, tag: &str) -> Result<()> {
    let tag = normalize_tag(tag)?;
    if !manifest.is_file() {
        return Err(format!("manifest does not exist: {}", manifest.display()).into());
    }
    ensure_tag_exists(&tag)?;

    let plain_version = tag.trim_start_matches('v');
    let commit = command_stdout("git", ["rev-list", "-n", "1", &tag])?
        .trim()
        .to_owned();
    let cargo_toml = command_stdout("git", ["show", &format!("{tag}:Cargo.toml")])?;
    let metainfo = command_stdout(
        "git",
        [
            "show",
            &format!("{tag}:data/io.github.screwys.Rufin.metainfo.xml"),
        ],
    )?;
    let cargo_version = workspace_version_from_cargo_toml(&cargo_toml)?;
    let metainfo_version = first_metainfo_release_version(&metainfo)?;

    if cargo_version != plain_version {
        return Err(format!(
            "tag {tag} has Cargo version {cargo_version}, expected {plain_version}"
        )
        .into());
    }
    if metainfo_version != plain_version {
        return Err(format!(
            "tag {tag} has MetaInfo release {metainfo_version}, expected {plain_version}"
        )
        .into());
    }

    let input = read_to_string(manifest)?;
    let value: serde_json::Value = serde_json::from_str(&input)?;
    let modules = value
        .get("modules")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| format!("{} missing modules array", manifest.display()))?;
    let rufin = modules
        .iter()
        .find(|module| module.get("name").and_then(serde_json::Value::as_str) == Some("rufin"))
        .ok_or_else(|| format!("{} missing rufin module", manifest.display()))?;
    let sources = rufin
        .get("sources")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| format!("{} rufin module missing sources array", manifest.display()))?;
    if sources.is_empty() {
        return Err(format!("{} rufin module has no sources", manifest.display()).into());
    }

    let output = update_flathub_manifest_source_text(&input, &tag, &commit)?;
    write_string(manifest, &output)?;
    println!("Updated {} to {} ({})", manifest.display(), tag, commit);
    Ok(())
}

fn create_tag(mut args: Vec<String>) -> Result<()> {
    let mut base_tag = String::new();
    let mut dry_run = false;
    let mut replace_tag = false;
    let mut skip_flathub = false;

    while !args.is_empty() {
        match args.remove(0).as_str() {
            "--base" => {
                if args.is_empty() {
                    return Err("--base requires a tag".into());
                }
                base_tag = normalize_tag(&args.remove(0))?;
            }
            "--dry-run" => dry_run = true,
            "--replace" => replace_tag = true,
            "--skip-flathub" => skip_flathub = true,
            "-h" | "--help" => {
                eprintln!(
                    "Usage: cargo run --locked -p xtask -- release create-tag [--base TAG] [--dry-run] [--replace] [--skip-flathub] VERSION SUMMARY"
                );
                return Ok(());
            }
            "--" => break,
            arg if arg.starts_with('-') => return Err(format!("unknown option: {arg}").into()),
            arg => {
                args.insert(0, arg.to_owned());
                break;
            }
        }
    }

    if args.len() != 2 {
        return Err("release create-tag requires VERSION and SUMMARY".into());
    }

    let version = normalize_tag(&args[0])?;
    let plain_version = version.trim_start_matches('v').to_owned();
    let summary = args[1].clone();
    if summary.is_empty() {
        return Err("release notes are required".into());
    }

    let root = repo_root()?;
    env::set_current_dir(&root)?;

    if !dry_run && !replace_tag && git_ref_exists(&format!("refs/tags/{version}"))? {
        return Err(format!("tag already exists: {version}").into());
    }

    if base_tag.is_empty() {
        base_tag = if replace_tag && git_ref_exists(&format!("refs/tags/{version}"))? {
            latest_release_tag_at(&format!("{version}^"))?
        } else {
            latest_release_tag_at("HEAD")?
        }
        .ok_or("could not find previous v* tag; pass --base TAG")?;
    }
    ensure_tag_exists(&base_tag)?;

    if !dry_run && !working_tree_clean()? {
        return Err("working tree must be clean before creating a release tag".into());
    }

    let commit_count =
        command_stdout("git", ["rev-list", "--count", &format!("{base_tag}..HEAD")])?;
    if commit_count.trim() == "0" {
        return Err(format!("no commits found in range {base_tag}..HEAD").into());
    }

    let mut notes = release_notes(&base_tag, &version, &summary)?;
    print_notes(&notes);
    if dry_run {
        return Ok(());
    }

    prepare_version(&plain_version, &summary)?;
    generate::flatpak_sources(false)?;
    verify_nix_flake()?;
    if !working_tree_clean()? {
        git_add_existing(&[
            "Cargo.lock",
            "Cargo.toml",
            "README.md",
            "data/io.github.screwys.Rufin.metainfo.xml",
            ".github/ISSUE_TEMPLATE/bug_report.yml",
            "packaging/flatpak/cargo-sources.json",
            RPM_SPEC,
        ])?;
        run_command(
            "git",
            [
                "commit",
                "-m",
                &format!("release: publish prep for {version}"),
            ],
        )?;
    }

    notes = release_notes(&base_tag, &version, &summary)?;
    print_notes(&notes);

    if replace_tag && git_ref_exists(&format!("refs/tags/{version}"))? {
        run_command("git", ["tag", "-d", &version])?;
    }

    let notes_file = temp_path("release-notes.md");
    write_string(&notes_file, &notes)?;
    run_command(
        "git",
        [
            "tag",
            "-s",
            "--cleanup=verbatim",
            &version,
            "-F",
            notes_file
                .to_str()
                .ok_or("release notes path is not valid UTF-8")?,
        ],
    )?;
    let _ = std::fs::remove_file(notes_file);
    run_command("git", ["show", &version, "--no-patch"])?;

    let flathub_manifest = PathBuf::from(FLATHUB_MANIFEST);
    if !skip_flathub && flathub_manifest.is_file() {
        update_flathub_manifest_path(&flathub_manifest, &version)?;
        if path_has_diff(&flathub_manifest)? {
            run_command("git", ["add", FLATHUB_MANIFEST])?;
            run_command(
                "git",
                [
                    "commit",
                    "-m",
                    &format!("chore(flatpak): update Flathub manifest for {version}"),
                ],
            )?;
        }
    }

    Ok(())
}

fn latest_release_tag_at(revision: &str) -> Result<Option<String>> {
    let output = capture_command(
        "git",
        [
            "describe",
            "--tags",
            "--abbrev=0",
            "--match",
            "v[0-9]*",
            revision,
        ],
    )?;
    if output.status.success() {
        let tag = output.stdout.trim();
        if tag.is_empty() {
            Ok(None)
        } else {
            Ok(Some(tag.to_owned()))
        }
    } else {
        Ok(None)
    }
}

fn git_ref_exists(ref_name: &str) -> Result<bool> {
    let output = capture_command("git", ["rev-parse", "-q", "--verify", ref_name])?;
    Ok(output.status.success())
}

fn working_tree_clean() -> Result<bool> {
    let diff = Command::new("git").args(["diff", "--quiet"]).status()?;
    let cached = Command::new("git")
        .args(["diff", "--cached", "--quiet"])
        .status()?;
    Ok(diff.success() && cached.success())
}

fn path_has_diff(path: &Path) -> Result<bool> {
    let status = Command::new("git")
        .args(["diff", "--quiet", "--"])
        .arg(path)
        .status()?;
    Ok(!status.success())
}

fn git_add_existing(paths: &[&str]) -> Result<()> {
    for path in paths {
        if Path::new(path).exists() {
            run_command("git", ["add", *path])?;
        }
    }
    Ok(())
}

fn verify_nix_flake() -> Result<()> {
    if !Path::new("flake.nix").is_file() {
        return Ok(());
    }
    run_command(
        "env",
        [
            "-u",
            "LD_PRELOAD",
            "nix",
            "--accept-flake-config",
            "--extra-experimental-features",
            "nix-command flakes",
            "flake",
            "check",
            "--no-build",
            "--no-write-lock-file",
            "--print-build-logs",
        ],
    )
}

fn release_notes(base_tag: &str, version: &str, summary: &str) -> Result<String> {
    let repo_url = repo_url_from_origin()?.unwrap_or_default();
    let repo_slug = github_repo_from_origin()?.unwrap_or_default();
    if repo_url.is_empty() || repo_slug.is_empty() {
        return Err("could not determine GitHub repository from origin".into());
    }

    let target_commitish = release_notes_target_commitish()?;
    let generated =
        github_generated_release_notes(&repo_slug, base_tag, version, &target_commitish)?;

    release_notes_from_generated_body(
        &repo_slug,
        &repo_url,
        base_tag,
        version,
        summary,
        &generated,
        |pr_number, pr_author| release_note_extra_authors_for_pr(&repo_slug, pr_number, pr_author),
    )
}

fn print_notes(notes: &str) {
    println!("Release notes (Markdown)");
    println!();
    print!("{notes}");
}

struct GeneratedReleaseNoteEntry {
    number: String,
    title: String,
    author: String,
}

fn release_notes_target_commitish() -> Result<String> {
    let log = command_stdout(
        "git",
        ["log", "--first-parent", "--format=%H%x09%s", "HEAD"],
    )?;
    for line in log.lines() {
        let Some((commit, subject)) = line.split_once('\t') else {
            continue;
        };
        if !is_release_notes_target_housekeeping_subject(subject) {
            return Ok(commit.to_owned());
        }
    }
    Err("could not find a non-release commit for generated release notes".into())
}

fn github_generated_release_notes(
    repo_slug: &str,
    base_tag: &str,
    version: &str,
    target_commitish: &str,
) -> Result<String> {
    if !find_on_path("gh") {
        return Err("gh is required to generate release notes".into());
    }

    let tag_field = format!("tag_name={version}");
    let previous_tag_field = format!("previous_tag_name={base_tag}");
    let target_field = format!("target_commitish={target_commitish}");
    let endpoint = format!("repos/{repo_slug}/releases/generate-notes");
    let output = capture_command(
        "gh",
        [
            "api",
            "--method",
            "POST",
            endpoint.as_str(),
            "-f",
            tag_field.as_str(),
            "-f",
            previous_tag_field.as_str(),
            "-f",
            target_field.as_str(),
            "--jq",
            ".body",
        ],
    )?;
    if !output.status.success() {
        eprint!("{}", output.stderr);
        return Err(format!("failed to generate GitHub release notes for {version}").into());
    }

    let body = output.stdout.trim();
    if body.is_empty() {
        return Err(format!("GitHub generated no release notes for {version}").into());
    }
    Ok(body.to_owned())
}

fn release_note_extra_authors_for_pr(
    repo_slug: &str,
    pr_number: &str,
    pr_author: &str,
) -> Result<Vec<String>> {
    let output = capture_command(
        "gh",
        [
            "pr",
            "view",
            pr_number,
            "--repo",
            repo_slug,
            "--json",
            "commits",
            "--jq",
            ".commits[] | .authors[] | .login // empty",
        ],
    )?;
    if !output.status.success() {
        eprint!("{}", output.stderr);
        return Err(format!("failed to inspect release note PR #{pr_number}").into());
    }
    let mut seen = HashSet::new();
    let mut authors = Vec::new();
    for author in output.stdout.lines().filter(|line| !line.is_empty()) {
        if author == pr_author || is_release_note_bot_author(author) || !seen.insert(author) {
            continue;
        }
        authors.push(author.to_owned());
    }
    Ok(authors)
}

fn release_notes_from_generated_body<F>(
    repo_slug: &str,
    repo_url: &str,
    base_tag: &str,
    version: &str,
    summary: &str,
    generated_body: &str,
    mut extra_authors_for_pr: F,
) -> Result<String>
where
    F: FnMut(&str, &str) -> Result<Vec<String>>,
{
    let entries = generated_changelog_entries(repo_slug, generated_body)?;
    let mut notes = String::new();
    notes.push_str(summary);
    notes.push_str("\n\n## Changelog\n\n");
    let mut wrote_entry = false;

    for entry in entries {
        if is_release_note_housekeeping_title(&entry.title) {
            continue;
        }

        let mut author_display = format_release_note_author(&entry.author);
        if is_release_note_bot_author(&entry.author) {
            for extra_author in extra_authors_for_pr(&entry.number, &entry.author)? {
                author_display.push_str(", ");
                author_display.push_str(&format_release_note_author(&extra_author));
            }
        }

        notes.push_str(&format!(
            "- {} by {} in #{}\n",
            entry.title, author_display, entry.number
        ));
        wrote_entry = true;
    }

    if !wrote_entry {
        return Err("release notes contain no public changelog entries".into());
    }

    notes.push_str(&format!(
        "\n**Full Changelog:** [{base_tag}...{version}]({repo_url}/compare/{base_tag}...{version})\n"
    ));
    notes.push('\n');
    Ok(notes)
}

fn generated_changelog_entries(
    repo_slug: &str,
    generated_body: &str,
) -> Result<Vec<GeneratedReleaseNoteEntry>> {
    let mut entries = Vec::new();
    let mut in_changes = false;
    let mut saw_changes = false;

    for line in generated_body.lines() {
        if line.trim() == "## What's Changed" {
            in_changes = true;
            saw_changes = true;
            continue;
        }

        if !in_changes {
            continue;
        }

        if line.starts_with("## ") || line.starts_with("**Full Changelog**") {
            break;
        }

        if line.trim().is_empty() {
            continue;
        }

        entries.push(
            parse_generated_changelog_line(repo_slug, line)
                .ok_or("unexpected GitHub release note format")?,
        );
    }

    if !saw_changes {
        return Err("GitHub release notes missing What's Changed".into());
    }
    if entries.is_empty() {
        return Err("GitHub release notes missing changelog entries".into());
    }
    Ok(entries)
}

fn parse_generated_changelog_line(
    repo_slug: &str,
    line: &str,
) -> Option<GeneratedReleaseNoteEntry> {
    let entry = line.strip_prefix("* ")?;
    let pr_url_prefix = format!("https://github.com/{repo_slug}/pull/");
    let (before_url, number) = entry.rsplit_once(&pr_url_prefix)?;
    let before_url = before_url.strip_suffix(" in ")?;
    let (title, author) = before_url.rsplit_once(" by ")?;
    let number = number.trim();
    if number.is_empty() || !number.chars().all(|ch| ch.is_ascii_digit()) {
        return None;
    }

    Some(GeneratedReleaseNoteEntry {
        number: number.to_owned(),
        title: title.to_owned(),
        author: parse_generated_author(author)?,
    })
}

fn parse_generated_author(author: &str) -> Option<String> {
    let author = author.trim();
    if let Some(login) = author.strip_prefix('@') {
        if login.is_empty() || login.contains(char::is_whitespace) {
            return None;
        }
        return Some(login.to_owned());
    }

    if let Some(rest) = author.strip_prefix("[@")
        && let Some((login, _)) = rest.split_once("](")
        && !login.is_empty()
    {
        return Some(login.to_owned());
    }

    None
}

fn format_release_note_author(author: &str) -> String {
    if let Some(app_slug) = author.strip_suffix("[bot]") {
        format!("[@{app_slug}](https://github.com/apps/{app_slug})")
    } else {
        format!("@{author}")
    }
}

fn is_release_note_bot_author(author: &str) -> bool {
    author.ends_with("[bot]") || author == "weblate"
}

fn is_release_publish_pr_title(title: &str) -> bool {
    title.starts_with("release: publish prep for v")
        || title.starts_with("chore(release): publish v")
}

fn is_release_note_housekeeping_title(title: &str) -> bool {
    is_release_publish_pr_title(title) || is_release_housekeeping_subject(title)
}

fn is_release_notes_target_housekeeping_subject(subject: &str) -> bool {
    is_release_publish_pr_title(subject)
        || subject.starts_with("chore(flatpak): update Flathub manifest for v")
        || subject.starts_with("chore(aur): update stable package for v")
        || subject.starts_with("release: publish stable packages for v")
        || subject.starts_with("release: sync stable package metadata for v")
}

fn is_release_housekeeping_subject(subject: &str) -> bool {
    subject.starts_with("chore(release): bump version to ")
        || subject.starts_with("release: publish prep for v")
        || subject.starts_with("chore(flatpak): update Flathub manifest for v")
        || subject.starts_with("chore(aur): update stable package for v")
        || subject.starts_with("release: publish stable packages for v")
        || subject.starts_with("release: sync stable package metadata for v")
        || subject.starts_with("Merge pull request #")
}

fn update_flathub_manifest_source_text(input: &str, tag: &str, commit: &str) -> Result<String> {
    let name_index = input
        .find("\"name\": \"rufin\"")
        .ok_or("manifest text missing rufin module name")?;
    let sources_offset = input[name_index..]
        .find("\"sources\": [")
        .ok_or("manifest text missing rufin sources")?;
    let sources_index = name_index + sources_offset;
    let object_offset = input[sources_index..]
        .find('{')
        .ok_or("manifest text missing first rufin source object")?;
    let object_start = sources_index + object_offset;
    let object_end = find_matching_json_object(input, object_start)?;

    let line_start = input[..object_start]
        .rfind('\n')
        .map_or(0, |index| index + 1);
    let indent = input[line_start..object_start]
        .chars()
        .take_while(|ch| ch.is_whitespace())
        .collect::<String>();
    let property_indent = format!("{indent}  ");
    let replacement = format!(
        "{{\n{property_indent}\"type\": \"git\",\n{property_indent}\"url\": \"https://github.com/screwys/Rufin.git\",\n{property_indent}\"tag\": \"{tag}\",\n{property_indent}\"commit\": \"{commit}\"\n{indent}}}"
    );

    let mut output = String::new();
    output.push_str(&input[..object_start]);
    output.push_str(&replacement);
    output.push_str(&input[object_end + 1..]);
    Ok(output)
}

fn find_matching_json_object(input: &str, object_start: usize) -> Result<usize> {
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;

    for (offset, ch) in input[object_start..].char_indices() {
        if in_string {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_string = false;
            }
            continue;
        }

        match ch {
            '"' => in_string = true,
            '{' => depth += 1,
            '}' => {
                depth = depth
                    .checked_sub(1)
                    .ok_or("manifest source object braces are unbalanced")?;
                if depth == 0 {
                    return Ok(object_start + offset);
                }
            }
            _ => {}
        }
    }

    Err("manifest source object is unterminated".into())
}

pub(crate) fn normalize_plain_version(input: &str) -> Result<String> {
    let version = input.strip_prefix('v').unwrap_or(input);
    if is_semverish(version) {
        Ok(version.to_owned())
    } else {
        Err("version must look like X.Y.Z".into())
    }
}

pub(crate) fn normalize_tag(input: &str) -> Result<String> {
    let tag = if input.starts_with('v') {
        input.to_owned()
    } else {
        format!("v{input}")
    };

    if is_semverish(tag.trim_start_matches('v')) {
        Ok(tag)
    } else {
        Err("tag must look like vX.Y.Z".into())
    }
}

fn is_semverish(version: &str) -> bool {
    let mut parts = version.splitn(3, '.');
    let Some(major) = parts.next() else {
        return false;
    };
    let Some(minor) = parts.next() else {
        return false;
    };
    let Some(patch_and_suffix) = parts.next() else {
        return false;
    };

    if major.is_empty()
        || minor.is_empty()
        || !major.chars().all(|ch| ch.is_ascii_digit())
        || !minor.chars().all(|ch| ch.is_ascii_digit())
    {
        return false;
    }

    let patch = patch_and_suffix
        .split(['-', '.'])
        .next()
        .unwrap_or_default();
    !patch.is_empty()
        && patch.chars().all(|ch| ch.is_ascii_digit())
        && patch_and_suffix
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '.')
}

fn ensure_tag_exists(tag: &str) -> Result<()> {
    command_stdout(
        "git",
        ["rev-parse", "-q", "--verify", &format!("refs/tags/{tag}")],
    )?;
    Ok(())
}

fn replace_workspace_version(version: &str) -> Result<()> {
    let path = PathBuf::from("Cargo.toml");
    let input = read_to_string(&path)?;
    let output = replace_workspace_version_in_toml(&input, version)?;
    write_string(&path, &output)
}

fn update_rpm_spec_version(version: &str) -> Result<()> {
    let path = PathBuf::from(RPM_SPEC);
    let input = read_to_string(&path)?;
    let output = update_rpm_spec_version_in(&input, version)?;
    write_string(&path, &output)
}

fn update_rpm_spec_version_in(input: &str, version: &str) -> Result<String> {
    let mut output = String::new();
    let mut replaced = false;

    for line in input.lines() {
        if line.starts_with("Version:") {
            output.push_str(&format!("Version:        {version}\n"));
            replaced = true;
        } else {
            output.push_str(line);
            output.push('\n');
        }
    }

    if replaced {
        Ok(output)
    } else {
        Err("missing RPM spec version".into())
    }
}

fn replace_workspace_version_in_toml(input: &str, version: &str) -> Result<String> {
    let mut output = String::new();
    let mut in_workspace_package = false;
    let mut replaced = false;

    for line in input.lines() {
        if line.starts_with('[') {
            in_workspace_package = line == "[workspace.package]";
        }

        if in_workspace_package && line.starts_with("version = ") {
            output.push_str(&format!("version = \"{version}\"\n"));
            replaced = true;
        } else {
            output.push_str(line);
            output.push('\n');
        }
    }

    if replaced {
        Ok(output)
    } else {
        Err("missing workspace package version".into())
    }
}

fn update_metainfo_release(version: &str, release_date: &str, notes: &str) -> Result<()> {
    let path = PathBuf::from("data/io.github.screwys.Rufin.metainfo.xml");
    let input = read_to_string(&path)?;
    let without_existing = remove_existing_release_entries(&input, version);
    let entry = format_metainfo_release(version, release_date, notes);
    let Some(index) = without_existing.find("  <releases>\n") else {
        return Err("missing releases section".into());
    };
    let insert_at = index + "  <releases>\n".len();
    let mut output = String::new();
    output.push_str(&without_existing[..insert_at]);
    output.push_str(&entry);
    output.push_str(&without_existing[insert_at..]);
    output = replace_raw_data_refs(&output, version);
    write_string(&path, &output)
}

fn remove_existing_release_entries(input: &str, version: &str) -> String {
    let mut output = String::new();
    let mut rest = input;
    let self_closing = format!("    <release version=\"{version}\"");

    loop {
        let Some(start) = rest.find(&self_closing) else {
            output.push_str(rest);
            break;
        };
        output.push_str(&rest[..start]);
        let candidate = &rest[start..];
        if let Some(line_end) = candidate.find('\n') {
            let line = &candidate[..line_end + 1];
            if line.trim_end().ends_with("/>") {
                rest = &candidate[line_end + 1..];
                continue;
            }
        }
        if let Some(end) = candidate.find("    </release>\n") {
            rest = &candidate[end + "    </release>\n".len()..];
        } else {
            output.push_str(candidate);
            break;
        }
    }

    output
}

fn format_metainfo_release(version: &str, release_date: &str, notes: &str) -> String {
    let mut blocks = Vec::new();
    let mut paragraph = Vec::new();
    let mut list = Vec::new();

    for line in notes.lines() {
        let line = line.trim_end();
        if let Some(item) = line.trim_start().strip_prefix("- ") {
            flush_paragraph(&mut paragraph, &mut blocks);
            let item = strip_issue_refs(item);
            if !item.is_empty() {
                list.push(item);
            }
        } else if line.trim().is_empty() {
            flush_paragraph(&mut paragraph, &mut blocks);
            flush_list(&mut list, &mut blocks);
        } else {
            flush_list(&mut list, &mut blocks);
            let item = strip_issue_refs(line);
            if !item.is_empty() {
                paragraph.push(item);
            }
        }
    }

    flush_paragraph(&mut paragraph, &mut blocks);
    flush_list(&mut list, &mut blocks);

    format!(
        "    <release version=\"{version}\" date=\"{release_date}\">\n      <description>\n{}      </description>\n    </release>\n",
        blocks.join("")
    )
}

fn flush_paragraph(paragraph: &mut Vec<String>, blocks: &mut Vec<String>) {
    if paragraph.is_empty() {
        return;
    }
    blocks.push(format!(
        "        <p>{}</p>\n",
        xml_escape(&paragraph.join("\n"))
    ));
    paragraph.clear();
}

fn flush_list(list: &mut Vec<String>, blocks: &mut Vec<String>) {
    if list.is_empty() {
        return;
    }
    blocks.push("        <ul>\n".to_owned());
    for item in list.drain(..) {
        blocks.push(format!("          <li>{}</li>\n", xml_escape(&item)));
    }
    blocks.push("        </ul>\n".to_owned());
}

fn strip_issue_refs(input: &str) -> String {
    let mut output = input.trim_end().to_owned();
    loop {
        let trimmed = output.trim_end();
        let Some((prefix, token)) = trimmed.rsplit_once(' ') else {
            break;
        };
        let token = token.trim_end_matches(['.', ',', ';', ':']);
        if is_issue_ref_token(token) {
            output = prefix.trim_end().to_owned();
        } else {
            break;
        }
    }
    output
}

fn is_issue_ref_token(token: &str) -> bool {
    let token = token
        .strip_prefix('(')
        .and_then(|value| value.strip_suffix(')'))
        .unwrap_or(token);
    let Some(hash) = token.rfind('#') else {
        return false;
    };
    let (repo, number) = token.split_at(hash);
    let number = &number[1..];
    !number.is_empty()
        && number.chars().all(|ch| ch.is_ascii_digit())
        && (repo.is_empty()
            || repo
                .split('/')
                .all(|part| !part.is_empty() && part.chars().all(is_repo_ref_char)))
}

fn is_repo_ref_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || ch == '_' || ch == '.' || ch == '-'
}

fn xml_escape(input: &str) -> String {
    input
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn replace_raw_data_refs(input: &str, version: &str) -> String {
    let mut output = String::new();
    let needle = "https://raw.githubusercontent.com/screwys/Rufin/";
    let mut index = 0;

    while let Some(offset) = input[index..].find(needle) {
        let start = index + offset;
        output.push_str(&input[index..start + needle.len()]);
        let after_ref = start + needle.len();
        if let Some(data_offset) = input[after_ref..].find("/data/") {
            output.push_str(&format!("v{version}"));
            index = after_ref + data_offset;
        } else {
            output.push_str(&input[after_ref..]);
            return output;
        }
    }

    output.push_str(&input[index..]);
    output
}

fn update_issue_template_versions(version: &str) -> Result<()> {
    let path = PathBuf::from(".github/ISSUE_TEMPLATE/bug_report.yml");
    let input = read_to_string(&path)?;
    let output = update_issue_template_versions_in(&input, version)?;
    write_string(&path, &output)
}

fn update_issue_template_versions_in(input: &str, version: &str) -> Result<String> {
    let lines = input.lines().collect::<Vec<_>>();
    let Some(id_index) = lines
        .iter()
        .position(|line| line.trim() == "id: rufin-version")
    else {
        return Err("missing issue template Rufin version dropdown".into());
    };
    let Some(options_index) = lines[id_index..]
        .iter()
        .position(|line| line.trim() == "options:")
        .map(|offset| id_index + offset)
    else {
        return Err("missing issue template Rufin version options".into());
    };

    let mut end = options_index + 1;
    while end < lines.len() && lines[end].trim_start().starts_with("- ") {
        end += 1;
    }

    let mut versions = vec![version.to_owned()];
    for line in &lines[options_index + 1..end] {
        let value = line
            .trim()
            .strip_prefix("- ")
            .unwrap_or_default()
            .trim_end_matches(LATEST_RELEASE_SUFFIX);
        if value != version && is_semverish(value) && !versions.iter().any(|seen| seen == value) {
            versions.push(value.to_owned());
            if versions.len() == 6 {
                break;
            }
        }
    }

    let mut output = String::new();
    for line in &lines[..=options_index] {
        output.push_str(line);
        output.push('\n');
    }
    for (index, entry) in versions.into_iter().enumerate() {
        output.push_str("        - ");
        output.push_str(&entry);
        if entry == version {
            output.push_str(LATEST_RELEASE_SUFFIX);
        }
        output.push('\n');
        if index == 0 {
            output.push_str("        - ");
            output.push_str(DEVELOPMENT_VERSION);
            output.push('\n');
        }
    }
    for line in &lines[end..] {
        output.push_str(line);
        output.push('\n');
    }
    Ok(output)
}

pub(crate) fn workspace_version_from_cargo_toml(input: &str) -> Result<String> {
    let mut in_workspace_package = false;
    for line in input.lines() {
        if line.starts_with('[') {
            in_workspace_package = line == "[workspace.package]";
        }
        if in_workspace_package && let Some(value) = quoted_value(line, "version") {
            return Ok(value);
        }
    }
    Err("missing workspace package version".into())
}

pub(crate) fn first_metainfo_release_version(input: &str) -> Result<String> {
    for line in input.lines() {
        let Some(start) = line.find("<release version=\"") else {
            continue;
        };
        let value_start = start + "<release version=\"".len();
        if let Some(end) = line[value_start..].find('"') {
            return Ok(line[value_start..value_start + end].to_owned());
        }
    }
    Err("missing MetaInfo release".into())
}

fn quoted_value(line: &str, key: &str) -> Option<String> {
    let prefix = format!("{key} = \"");
    line.strip_prefix(&prefix)
        .and_then(|value| value.strip_suffix('"'))
        .map(ToOwned::to_owned)
}
