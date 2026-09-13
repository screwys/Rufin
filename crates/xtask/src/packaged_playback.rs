use std::env;
use std::fs::{self, File};
use std::path::Path;
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::Result;
use crate::process::command_stdout;

pub(crate) fn run(args: Vec<String>) -> Result<()> {
    let usage = "Usage: cargo run --locked -p xtask -- verify packaged-playback EXECUTABLE";
    if matches!(args.as_slice(), [arg] if arg == "--help" || arg == "-h") {
        println!("{usage}");
        return Ok(());
    }
    let [executable] = args.as_slice() else {
        return Err(usage.into());
    };
    if env::var_os("CI").is_none() {
        return Err("This check adds a source; use a disposable CI account".into());
    }
    let executable = fs::canonicalize(executable)?;
    let directories = directories::ProjectDirs::from("io.github", "screwys", "Rufin")
        .ok_or("Could not locate Rufin's configuration directory")?;
    let config = directories.config_dir().join("settings.json");
    let previous = match fs::read(&config) {
        Ok(bytes) => Some(bytes),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(error.into()),
    };
    let mut settings: Value = previous
        .as_deref()
        .map(serde_json::from_slice)
        .transpose()?
        .unwrap_or_else(|| json!({}));
    settings["playback"]["audio_output"] = json!("fakesink");
    settings["secret_storage_mode"] = json!("config-file");
    fs::create_dir_all(directories.config_dir())?;
    let result = (|| {
        fs::write(&config, serde_json::to_vec(&settings)?)?;
        check(&executable)
    })();
    if let Some(previous) = previous {
        fs::write(config, previous)?;
    } else if config.exists() {
        fs::remove_file(config)?;
    }
    result
}

fn check(executable: &Path) -> Result<()> {
    let work = tempfile::tempdir()?;
    let music = work.path().join("Music with spaces – 音楽");
    fs::create_dir(&music)?;
    crate::media::playback_fixture(&music)?;
    let token = crate::process::temp_path("playback-token")
        .file_name()
        .ok_or("Missing test token")?
        .to_string_lossy()
        .into_owned();
    let stdout = work.path().join("stdout");
    let stderr = work.path().join("stderr");
    let mut command = Command::new(executable);
    command.args(["--headless", "--listen", "127.0.0.1:0"]);
    for (key, _) in env::vars_os() {
        if ["GST_", "DYLD_", "LD_LIBRARY_", "GIO_", "GDK_PIXBUF_"]
            .iter()
            .any(|prefix| key.to_string_lossy().starts_with(prefix))
        {
            command.env_remove(key);
        }
    }
    #[cfg(windows)]
    let path = {
        let system =
            std::path::PathBuf::from(env::var_os("SystemRoot").ok_or("Missing SystemRoot")?);
        env::join_paths([system.join("System32"), system])?
    };
    #[cfg(not(windows))]
    let path = "/usr/bin:/bin:/usr/sbin:/sbin";
    let mut child = command
        .env("PATH", path)
        .env("RUFIN_API_TOKEN", &token)
        .env("GST_REGISTRY_1_0", work.path().join("registry.bin"))
        .env("RUFIN_GST_REGISTRY_1_0", work.path().join("registry.bin"))
        .stdout(File::create(&stdout)?)
        .stderr(File::create(&stderr)?)
        .spawn()?;
    let result = (|| {
        let deadline = Instant::now() + Duration::from_secs(30);
        let address = loop {
            let output = fs::read_to_string(&stdout)?;
            if let Some(address) = output
                .lines()
                .find_map(|line| line.strip_prefix("Rufin API listening on "))
            {
                break address.to_owned();
            }
            if child.try_wait()?.is_some() || Instant::now() >= deadline {
                return Err("The packaged headless server did not start".into());
            }
            thread::sleep(Duration::from_millis(100));
        };
        let source = api(
            &address,
            &token,
            "/api/sources/local",
            Some(json!({"paths": [music]})),
        )?;
        let id = source["id"].as_str().ok_or("Source response has no ID")?;
        let response = api(&address, &token, &format!("/api/tracks?source={id}"), None)?;
        let tracks = response["tracks"]
            .as_array()
            .ok_or("Missing scanned tracks")?;
        if tracks.len() != 1 {
            return Err(format!("Expected one scanned track, got {}", tracks.len()).into());
        }
        api(
            &address,
            &token,
            "/api/queue",
            Some(json!({"uris": [tracks[0]["uri"]], "mode": "replace"})),
        )?;
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            let status = api(&address, &token, "/api/playback", None)?;
            if !status["error"].is_null() {
                return Err(format!("Playback failed: {}", status["error"]).into());
            }
            if status["state"] == "playing" && status["position_ms"].as_u64().unwrap_or(0) >= 500 {
                break;
            }
            if Instant::now() >= deadline {
                return Err("The scanned track did not reach advancing playback".into());
            }
            thread::sleep(Duration::from_millis(100));
        }
        api(&address, &token, "/api/playback/stop", Some(json!({})))?;
        println!("Installed Unicode file discovery and headless playback passed.");
        Ok(())
    })();
    let _ = child.kill();
    let _ = child.wait();
    if result.is_err() {
        eprintln!("{}", fs::read_to_string(stderr).unwrap_or_default());
    }
    result
}

fn api(address: &str, token: &str, path: &str, data: Option<Value>) -> Result<Value> {
    let mut args = vec![
        "--silent".to_owned(),
        "--show-error".to_owned(),
        "--fail-with-body".to_owned(),
        "--max-time".to_owned(),
        "30".to_owned(),
        "--noproxy".to_owned(),
        "*".to_owned(),
        "--header".to_owned(),
        format!("Authorization: Bearer {token}"),
        "--header".to_owned(),
        "Content-Type: application/json".to_owned(),
    ];
    if let Some(data) = data {
        args.extend(["--data-binary".to_owned(), serde_json::to_string(&data)?]);
    }
    args.push(format!("{address}{path}"));
    Ok(serde_json::from_str(&command_stdout("curl", args)?)?)
}
