# Contributing

Thank you for your interest in contributing to Rufin! 

Contributions are welcome for:

- Documentation: additions, screenshots (should follow [Flathub guidelines](https://docs.flathub.org/docs/for-app-authors/metainfo-guidelines/quality-guidelines)) and more
- Packaging: editing current methods or adding new ones; `xtask` crate should be checked
- Bug fixes: if you want to patch a bug, please clearly explain how to reproduce it in your PR
- New features: You can open an issue/discussion beforehand unless you want to make it a surprise!
- UX improvements

For translations, you can visit [Weblate](https://hosted.weblate.org/projects/rufin/app/) instead.

## Development environment

For development using your host packages, please see
[README.md#building-locally](README.md#building-locally).

If you have nix available, it is easier to:

```bash
git clone https://github.com/screwys/Rufin.git
cd Rufin
nix develop
```

The cache for `main` and release tags is available through Cachix:

```bash
nix-shell -p cachix --run "cachix use rufin"
```

If you do not want to install Linux dependencies on your host or do not have
Nix available, Rufin also provides a minimal Fedora container that enters the
same Nix development shell. It can run checks and build the Linux app, Arch
package, Flatpak, and RPMs, but it cannot start Rufin or build the native macOS
and Windows packages.

```bash
just container setup
```

## Project structure

Rufin's crates try to follow a product ownership model. The goal is to separate parts that can grow vertically (or parts we want to scale) and work on them independently, while only doing minimal or no work for their integration with other parts.

| Crate | What it is for |
| :--- | :--- |
| `app-identity` | single-file crate to separate development builds |
| `artwork` | artwork selection, loading, and caching |
| `desktop-integration` | MPRIS, notifications, the tray, and Discord RPC |
| `downloads` | server track downloads and download management |
| `library` | music items, listening activity, and the database |
| `localization` | translation tooling and locales |
| `lyrics` | lyrics fetching, selection, and state |
| `metadata-lookup` | external metadata and artwork lookups |
| `playback` | playback behavior and the queue |
| `playback-cast` | UPnP, Chromecast and AirPlay casting integration |
| `playback-gstreamer` | the GStreamer playback backend |
| `rufin` | app startup, settings persistence, and crate composition |
| `scrobbling` | scrobbling services|
| `secrets` | storage for credentials and service keys |
| `sources` | source-specific operations |
| `ui` | GTK views and navigation|
| `windows-updater` | automatic windows updates from .exe |
| `xtask` | development and packaging commands |

## Development commands

```bash
just build # builds the native binary, macOS disk image, or Windows installer
just build arch # builds the Arch package
just build flatpak # builds the Flatpak
just build rpm # builds Fedora RPMs for x86_64
just build rpm arm # builds Fedora RPMs for AArch64
just clean # clears Rufin build state while keeping finished artifacts
just debug # runs the development app on the host
just fmt # formats Rust code
just test # runs the test suite
```

To run the broader testing suite:

```bash
just check
```

Run `just deps` after changing Linux package dependencies or AUR metadata; `just check deps`
validates the generated metadata. Direct `makepkg --printsrcinfo` also works on
Arch-based systems, while `just deps` handles a Nix-provided `makepkg` without
`/etc/makepkg.conf`.

If you are testing natively, this also needs rustfmt, clippy, cargo-deny, and gettext.
`cargo-nextest` and `ast-grep` (which CI runs by default) are used when available.

To enable the debug logging, refer to [README.md#troubleshooting](README.md#troubleshooting).

Most commands work the same for local and container development. Linux builds plus dependency,
formatting, test, and check commands use the host by default or the development container after it
is set up; macOS and Windows package builds always run on their matching host. Flatpak uses a
privileged container for nested sandboxing only during that command. RPM mounts the selected host
engine socket only during its command; that socket can control the engine. Disposable package work
is kept under `.local/build`, container state under `.local/container`, and finished artifacts under
`.local/artifacts`. Use `just container shell` for an interactive shell, `just container disable` to
return commands to the host, or `just container reset` to clear the container state. `just debug`
always runs on the host and is unavailable inside the container shell.

## Guidelines

For GTK work, please see GTK's
[Preparing for GTK 5](https://docs.gtk.org/gtk4/migrating-4to5.html) guide, as Rufin tries to remain compatible with GTK 5.

For commit names and PRs, we prefer
[Conventional Commits](https://www.conventionalcommits.org/en/v1.0.0/#summary) or [Scoped Commits](https://scopedcommits.com/).

Do not change strings without a strong motivation, since this means more work for translators. 

### Pull Requests

Although I do not hate surprises, it is preferable that a prior conversation was done before the PR, or it closes an issue. You can open such an issue yourself as well, which would likely save a lot of time for the both sides if we reach an agreement on the scope. If the change is straightforward enough that a direct PR saves more time, this can be skipped. 

Please keep your pull requests scoped. If it affects 2 unrelated areas, it is better to split PRs. We have a PR-centric workflow and usually most PRs have an issue they adress. Hence it might be to your benefit to use `git blame` and see if the code you are changing was adressing an issue, which helps to prevent a regression. 

Try to attach screenshots or examples if applicable. Talk about the motivation and what you tested, what you did not test and if you are unsure whether this affects something else. This greatly speeds up the review process by reducing the number of questions.

### LLM Policy

In general, respect humans. Conversation must be done between humans, I absolutely do not like reading any issue/comment and such written by an LLM. There is no requirement for perfect English, I would prefer reading broken English over verbose hallucinated text. If your English is not at the level you can describe the issue, you can use [a direct translation tool](https://libretranslate.com/) or at least tell your LLM to directly translate what you want to say, instead of sending an overly verbose wall of text.

I have no interest in hypothetical hardening suggestions or bugs found by LLMs. If you have encountered a bug, you are free (and encouraged) to open an issue; but do not ask an LLM to find bugs just to open an issue. This is derivable from the first sentence of the policy.

It is usually obvious to spot LLM generated code if the diff is large. If you use an LLM for contribution, then you are fully responsible of the code it generates, and you must make sure it fits these guidelines. LLMs should not edit any human facing part on its on own, absolutely not the translations. 

You should definitely not do any LLM advertisement, neither in conversations or commits. LLMs can not be held accountable, hence they must not sign anything off. 
