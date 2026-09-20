# About Threading Models: Async, Multi-Threaded, and Single-Threaded

## Introduction

When I took a first look at the SMB spec, I immediately noticed there's an async feature for message processing,
and along with the fact that it's an obvious I/O related crate, being a network protocol library,
it raises the question of how to handle concurrency and parallelism effectively.

I personally admire the concept of async I/O - in Python, C# and Rust, the implementations are different,
but all give the same performance advantage, and are all based on the same idea.
Having that in my mind, I also wanted to avoid forcing users to use async/await syntax if they don't want to -
for example, if their current application does not use async/await,
or if they just prefer having a smaller binary with simple threading,
or maybe even with no threading at all.

I did some research on this issue, and came across a [great blog post by NullDeref](<https://nullderef.com/blog/rust-async-sync/>),
suggesting to use a cool crate named [`maybe_async`]. This crate allows you to write async code lines,
but when the code is actually being compiled, depending on the configuration,
the crate can eliminate every `await` and `async`, and in fact, turn your code to a synchronous one. How cool!

So I did the obvious, and decided to allow users to have the option to choose between the three options:

* Async (using [`tokio`])
* Synchronous (using [`std::thread`])
* No threading (single-threaded)

As you use a "weaker" model, some features may not be available, especially when using a single-threaded application.

How does the code look like when using `maybe_async`? Let's check out the main usage example of using the crate.
Instead of the original example, which can be seen in [the main docs page][crate]: <!-- markdownlint-disable reference-links-images -->

```rust,no_run
use smb::{Client, ClientConfig, UncPath, FileCreateArgs, FileAccessMask};
use std::str::FromStr;

#[cfg(feature = "async")] // can also use [maybe_async::async_impl]
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    do_main().await
}

#[cfg(not(feature = "async"))] // can also use [maybe_async::sync_impl]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    do_main()
}

#[maybe_async::maybe_async]
async fn do_main() -> Result<(), Box<dyn std::error::Error>> {
    // instantiate the client
    let client = Client::new(ClientConfig::default());

    // Connect to a share
    let target_path = UncPath::from_str(r"\\server\share").unwrap();
    client.share_connect(&target_path, "username", "password".to_string()).await?;

    // And open a file on the server
    let file_to_open = target_path.with_path("file.txt");
    let read_access= FileAccessMask::new().with_generic_read(true);
    let file_open_args = FileCreateArgs::make_open_existing(read_access);
    let file = client.create_file(&file_to_open, &file_open_args).await?;
    // now, you can do a bunch of operations against `file`, and close it at the end.
    Ok(())
}
```

In this case, the main function is special, since we use `tokio::main` to invoke the async main through the tokio runtime. The same goes when implementing your code, and simple elimination of `async` and `await` is not enough to adapt the code from async to sync.

## Choosing a Threading Model

* For most use cases, the `async` model is the best. It provides the best performance and scalability, especially for I/O-bound tasks, it does not use too much system resources, and it allows for a more natural programming style when dealing with asynchronous operations.

* For use cases where you can't or won't use async/await, using `multi_threaded` is the next best option. It supports almost all the features as the async model.
* For use cases where you would like to keep things minimized, either in the aspect of resource usage - system resources and binary size, you might want to consider the `single_threaded` model.

Well, how do you select a specific threading model?
> ⚠️ By default, the `async` model is selected.

That makes sense, since we like async very much in this crate. But if you rather use any other kind of threading model, you may just specify that in the crate's `features` when using it.

For example, building the crate to use async, is as simple as:

```sh
cargo build
```

But to use the multi-threaded model, you would specify it like this:

```sh
cargo build --no-default-features --features "multi_threaded,sign,encrypt"
```

> ⚠️ Make sure to include other default crate features as needed when changing threading model!

The very same goes to `single_threaded`.

## Using the crate in different threading models

There is a good variety of using the crates in both the integrations test (see the [integration tests](https://github.com/afiffon/smb-rs/tree/main/smb/tests) directory), and in the [`smb_cli`](https://github.com/afiffon/smb-rs/tree/main/smb_cli) project.

For example, there's a good example of iterating a directory in either async or multi-threaded environment, in both the tests and the `smb_cli` project - One uses [`futures_core::Stream`], which are the closest way of describing an async iterator in rust, and the other uses a good old [`std::iter::Iterator`]-based implementation.
