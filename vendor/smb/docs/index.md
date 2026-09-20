# SMB

the `smb` crate is a pure rust SMB client, supports the SMB2 protocol (including SMB3).

## Basic usage

The most basic functionality that an SMB client should provide is the ability to
connect to an SMB server, authenticate, and perform simple file operations.

The [`Client`] struct provides a simple interface for interacting with an SMB server. Let's see how we use it.

```rust,no_run
use smb::{Client, ClientConfig, UncPath, FileCreateArgs, FileAccessMask};
use std::str::FromStr;
# #[cfg(not(feature = "async"))] fn main() {}

# #[cfg(feature = "async")]
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
    let file = client.create_file(&file_to_open, &file_open_args).await?;
    // now, you can do a bunch of operations against `file`, and close it at the end.
    Ok(())
}
```

Cool! we got ourselves a live connection to an SMB server, and we also got a file open.

But wait... How do we know it's actually a file? Well, we don't. The [`Client::create_file`] method returns the [`Resource`] struct, which is a union to a file, directory, or a pipe - the supported SMB resources in this crate (printers are currently not implemented specifically). What we need to do next, is to find out what type of resource we've got:

```rust,no_run
# use smb::*;
# #[cfg(not(feature = "async"))] fn main() {}
# #[tokio::main]
# #[cfg(feature = "async")]
# async fn main() -> Result<()> {
# let client = Client::default();
# let mut file = client.create_file(&UncPath::new("")?, &FileCreateArgs::default()).await?;
match &file {
    Resource::File(file) => {
        // We have a file
    }
    Resource::Directory(dir) => {
        // We have a directory
    }
    Resource::Pipe(pipe) => {
        // We have a pipe
    }
}
// Note: we could also use `.unwrap_file()` here,
// or similar method provided by Resource to find out what kind of resource this is!
# Ok(())}
```

Cool! Let's assume we got ourselves a file. Then, we can do the obvious operation of reading or writing a block of data from or to the file:

```rust,no_run
# use smb::*;
# #[cfg(not(feature = "async"))] fn main() {}
# #[cfg(feature = "async")]
# #[tokio::main]
# async fn main() -> Result<()> {
# let client = Client::default();
# let mut file = client.create_file(&UncPath::new("")?, &FileCreateArgs::default()).await?;
let file: File = file.unwrap_file();

let mut data: [u8; 1024] = [0; 1024];
file.read_at(&mut data, 0).await?;
file.write_at(&data, 0).await?;
# Ok(())}
```

At the end, close the file.

```rust,no_run
# use smb::*;
# #[cfg(not(feature = "async"))] fn main() {}
# #[cfg(feature = "async")]
# #[tokio::main]
# async fn main() -> Result<()> {
# let client = Client::default();
# let mut file = client.create_file(&UncPath::new("")?, &FileCreateArgs::default()).await?;
# let file: File = file.unwrap_file();
file.close().await?;
# Ok(())}
```

## Feature flags

| Type            | Algorithm           |  Async  | Multi-threaded | Single-threaded | Feature Name           |
| --------------- | ------------------- | ---| ---| --- | ---------------------- |
| Authentication  | Kerberos            | ✅  | ✅  | ✅   | `kerberos`             |
| Transport       | QUIC                | ✅  | ❌   | ❌    | `quic`                 |
| **Signing**     | *                   |    |    |     | `sign`                 |
| Signing         | HMAC_SHA256         | ✅  | ✅  | ✅   | `sign_hmac`            |
| Signing         | AES-128-GCM         | ✅  | ✅  | ✅   | `sign_gmac`            |
| Signing         | AES-128-CCM         | ✅  | ✅  | ✅   | `sign_cmac`            |
| **Encryption**  | *                   |    |    |     | `encrypt`              |
| Encryption      | AES-128-CCM         | ✅  | ✅  | ✅   | `encrypt_aes128ccm`    |
| Encryption      | AES-128-GCM         | ✅  | ✅  | ✅   | `encrypt_aes128gcm`    |
| Encryption      | AES-256-CCM         | ✅  | ✅  | ✅   | `encrypt_aes256ccm`    |
| Encryption      | AES-256-GCM         | ✅  | ✅  | ✅   | `encrypt_aes256gcm`    |
| **Compression** | *                   |    |    |     | `compress`             |
| Compression     | LZ4                 | ✅  | ✅  | ✅   | `compress_lz4`         |
| Compression     | Pattern_V1          | 🟡  | 🟡  | 🟡   | `compress_pattern_v1`* |
| Compression     | LZNT1/LZ77/+Huffman | ❌  | ❌  | ❌   | -                      |

* The Pattern_V1 compression algorithm currently supports in-bound decompression only.

## Advanced documentation
<!-- markdownlint-disable reference-links-images -->
* [Switching between threading models][docs::threading_models]

* [Performing parallel file operations][docs::parallelize]
<!-- markdownlint-enable reference-links-images -->
