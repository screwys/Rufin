# smb-rs: The SMB2 Client in Rust

[![Build](https://github.com/afiffon/smb-rs/actions/workflows/build.yml/badge.svg)](https://github.com/afiffon/smb-rs/actions/workflows/build.yml)
[![Crates.io](https://img.shields.io/crates/v/smb)](https://crates.io/crates/smb)
[![docs.rs](https://img.shields.io/docsrs/smb/latest?link=https%3A%2F%2Fdocs.rs%2Fsmb%2Flatest%2Fsmb%2Findex.html)](https://docs.rs/smb/latest/smb/index.html)

This project is the first rust implementation of
[SMB2 & 3](https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-smb2/5606ad47-5ee0-437a-817e-70c366052962) client --
the protocol that powers Windows file sharing and remote services.
The project is designed to be used as a crate, but also includes a CLI tool for basic operations.

While most current implementations are mostly bindings to C libraries (such as libsmb2, samba, or windows' own libraries), this project is a full implementation in Rust, with no _direct_ dependencies on C libraries.

## Getting started

Running the project's CLI is as simple as executing:

```sh
cargo run -- --help
```

Check out the `info` and the `copy` sub-commands for more information.

## Features

- ✅ All SMB 2.X & 3.X dialects support.
- ✅ Wire message parsing is fully safe, using the `binrw` crate.
- ✅ Async (`tokio`), Multi-threaded, or Single-threaded client.
- ✅ Compression & Encryption support.
- ✅ Transport using SMB over TCP (445), over NetBIOS (139), and over QUIC (443).
- ✅ NTLM & Kerberos authentication (using the [`sspi`](https://crates.io/crates/sspi) crate).
- ✅ Cross-platform (Windows, Linux, MacOS).

You are welcome to see the project's roadmap in the [GitHub Project](https://github.com/users/afiffon/projects/2).

## Using the crate

Check out the `Client` struct, exported from the `smb` crate, to initiate a connection to an SMB server:

```rust,no_run
use smb::{Client, ClientConfig, UncPath, FileCreateArgs, FileAccessMask, ReadAtChannel};
use std::str::FromStr;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // instantiate the client
    let client = Client::new(ClientConfig::default());

    // Connect to a share
    let target_path = UncPath::from_str(r"\\server\share").unwrap();
    client.share_connect(&target_path, "username", "password".to_string()).await?;

    // And open a file on the server
    let file_to_open = target_path.with_path("file.txt");
    let file_open_args = FileCreateArgs::make_open_existing(FileAccessMask::new().with_generic_read(true));
    let resource = client.create_file(&file_to_open, &file_open_args).await?;

    // now, you can do a bunch of operations against `file`, and close it at the end.
    let file = resource.unwrap_file();
    let mut data: [u8; 1024] = [0; 1024];
    file.read_at(&mut data, 0).await?;

    // and close
    file.close().await?;
    Ok(())
}
```

Check out the [docs.rs](https://docs.rs/smb/latest/smb/index.html) for more information regarding usage.

## Development

To set up a development environment, you may use any supported rust version.

- It is highly recommended to use rust nightly, and install pre-commit hooks (using `pip install pre-commit && pre-commit install`)
- Before committing your changes, run `cargo fmt` to format the code, and `cargo clippy` to check for linting issues.
- Run crate tests once you are ready to commit. Read tests' README.md before proceeding!
