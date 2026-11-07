# Security

## Security Stance

Rufin is officially supported as APK, Flatpak, Fedora RPM, nixpkgs/scoop/brew/AUR packages, and Windows/MacOS executables. Data security is taken seriously within the expectations of a music player. If you find a security or privacy issue, you can email to me@screwy.dev. You can also encrypt it with my [public key](https://screwy.dev/pgp.asc) if your mail client does not find the key automatically.

Rust dependencies are checked with cargo-deny, including advisories, license policy and source policy. Dependency updates are held for 7 days before our own Renovate bot opens them.

Release tags are signed via maintainer GPG key.

CI caches are only used to speed up builds. PRs can restore the Rust build cache, but cache writes are limited to pushes to `main`, and cached binaries are disabled. Nix/Cachix cache write is limited to `main` or releases, with releases verifying the signed release tag first.

## Stored Secrets

Rufin stores server tokens and service credentials on system keyring if possible, and falls back to a secrets file with owner only permissions on non-UNIX/unsupported systems or if the user deliberately picks legacy storage. Secrets file is still a reasonable choice for this app, that is the default in most clients since these are not considered as high-risk secrets. Secure storage exists as a good foundation, and useful in the case we need to store higher-value secrets in the future.

## Rufin Connect

Rufin Connect syncs a profile between your own devices. Devices talk to each other directly over QUIC with TLS 1.3, preferring post-quantum key exchange, and each one is identified by its own key, kept in the default secret storage (keyring by default). 

Server and scrobbling credentials belong to a profile key before they leave a device, and a sync folder only holds data encrypted to that key. Paired devices can read them, as with any synced setting.

Relay is disabled by default. A relay is used only after you enter its address, and Connect then contacts no other server for it; no public address directory is used. Once a relay is configured, the relay sees each device's IP address and forwards encrypted traffic it can not read. The relay address applies to every device in the profile.

## External Requests

Rufin, by design, comes with external lyric, metadata and cover lookups enabled, Discord Rich Presence disabled, and Last.fm/Libre.fm/ListenBrainz scrobbling disabled as well as they need to be authorized. Covers and metadata are looked up in bulk, they also help filling empty artist IDs in your tracks while always respecting your source metadata. Lyrics are cached lazily. We have a private mode that stops all of these external requests, on top of separate toggles for each of these, and it can be enabled during onboarding as well. Private mode still reports to your music server, therefore it can not control what your plugins do, and it also can not stop metadata providers configured in your server from working. Rufin talks to these services:

### Music servers

Your configured Jellyfin, Emby, Plex, Navidrome, or OpenSubsonic server, plus configured WebDAV and Samba shares. Again, Rufin can not control your server settings that do external requests on their own. Private mode keeps this path working.

### Metadata

MusicBrainz (default)

Last.fm (only if API key is configured)

Cover Art Archive (default, cover arts only)

MusicBrainz is used for most of the metadata. Cover Art Archive is used for cover images and some metadata. Last.fm is used as a cover lookup fallback when configured.

External site link settings in preferences only control whether Last.fm, MusicBrainz and server links are shown on detail pages. They do not control metadata lookup, and Last.fm links can be built from artist and album names without contacting Last.fm.

### Lyrics

LRCLIB (default)

Netease (default)

Genius (opt-in)

SimpMusic (opt-in)

Local .lrc files are read from disk and do not use the network. Server lyrics come from your configured music server.

Rufin's own external lyric lookup uses the providers above. Private mode disables Rufin external lyric lookup, while cached lyrics including previously fetched from external services still work.

### Scrobbling

Last.fm, Libre.fm and ListenBrainz are only used after authorization or configuration. Private mode disables playback reporting to these services.

### Discord Rich Presence

Discord Rich Presence is disabled by default. It may also use Last.fm, MusicBrainz or Cover Art Archive for cover images.

### Release notification

GitHub

Release notifications use GitHub release data. This can be disabled by private mode or a separate setting.

## Telemetry

Rufin itself does not have any way to collect telemetry. The services it talks to may have different privacy policies.

## Logs

Logs are privacy-conscious by default, you may still want to review them before sharing. They stay on-device, and are not uploaded to somewhere automatically. 
