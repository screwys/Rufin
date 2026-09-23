//! Translate source folder URIs at the native file-manager boundary.

use gtk::gio;
use url::Url;

pub async fn open_folder_uri(window: &gtk::ApplicationWindow, uri: &str) -> Result<(), String> {
    let url = Url::parse(uri).map_err(|error| error.to_string())?;
    #[cfg(target_os = "macos")]
    if matches!(url.scheme(), "http" | "https" | "smb") {
        // Finder handles network locations; the default HTTP handler is a browser.
        let process = gio::Subprocess::newv(
            &[
                "/usr/bin/open".as_ref(),
                "-a".as_ref(),
                "Finder".as_ref(),
                uri.as_ref(),
            ],
            gio::SubprocessFlags::NONE,
        )
        .map_err(|error| error.to_string())?;
        return process
            .wait_check_future()
            .await
            .map_err(|error| error.to_string());
    }
    #[cfg(target_os = "windows")]
    let file = match windows_network_path(&url)? {
        Some(path) => gio::File::for_path(path),
        None => gio::File::for_uri(uri),
    };
    #[cfg(not(target_os = "windows"))]
    let file = gio::File::for_uri(&file_manager_uri(&url));
    gtk::FileLauncher::new(Some(&file))
        .launch_future(Some(window))
        .await
        .map_err(|error| error.to_string())
}

#[cfg(any(not(target_os = "windows"), test))]
fn file_manager_uri(url: &Url) -> String {
    match url.scheme() {
        "http" => format!("dav{}", &url[url::Position::AfterScheme..]),
        "https" => format!("davs{}", &url[url::Position::AfterScheme..]),
        _ => url.to_string(),
    }
}

#[cfg(any(target_os = "windows", test))]
fn windows_network_path(url: &Url) -> Result<Option<String>, String> {
    if !matches!(url.scheme(), "smb" | "http" | "https") {
        return Ok(None);
    }
    let host = url
        .host_str()
        .ok_or_else(|| "Folder URL has no server".to_owned())?;
    let host = if host.starts_with('[') {
        format!(
            "{}.ipv6-literal.net",
            host.trim_matches(['[', ']']).replace(':', "-")
        )
    } else {
        host.to_owned()
    };
    let mut path = format!("\\\\{host}");
    if url.scheme() != "smb" {
        if url.scheme() == "https" {
            path.push_str("@SSL");
        }
        if let Some(port) = url.port() {
            path.push_str(&format!("@{port}"));
        }
        path.push_str("\\DavWWWRoot");
    }
    for part in url
        .path_segments()
        .into_iter()
        .flatten()
        .filter(|part| !part.is_empty())
    {
        path.push('\\');
        path.push_str(
            &gtk::glib::Uri::unescape_segment(Some(part), None, None)
                .ok_or_else(|| "Folder URL has an invalid path".to_owned())?,
        );
    }
    Ok(Some(path))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn network_folders_target_the_file_manager_on_each_desktop() {
        for (uri, gio_uri, windows_path) in [
            (
                "https://cloud.test:8443/dav/Artist%20One/",
                "davs://cloud.test:8443/dav/Artist%20One/",
                "\\\\cloud.test@SSL@8443\\DavWWWRoot\\dav\\Artist One",
            ),
            (
                "http://cloud.test/dav/",
                "dav://cloud.test/dav/",
                "\\\\cloud.test\\DavWWWRoot\\dav",
            ),
            (
                "smb://nas.test/music/Album%23One/",
                "smb://nas.test/music/Album%23One/",
                "\\\\nas.test\\music\\Album#One",
            ),
        ] {
            let url = Url::parse(uri).unwrap();
            assert_eq!(file_manager_uri(&url), gio_uri);
            assert_eq!(
                windows_network_path(&url).unwrap().as_deref(),
                Some(windows_path)
            );
        }
    }
}
