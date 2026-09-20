# Parallelization

One of the most challenging aspects of working with SMB is achieving efficient parallelization. This page review common facts and examples, showing how to properly parallelize SMB operations in your applications.

## Background: The SMB protocol

The SMB protocol, especially in newer SMB2 dialects, supports the "large MTU" feature. This allows clients to send multiple requests to the server simultaneously, without waiting for individual responses between requests. The mechanism is managed by a credit system: the client requests a number of credits from the server, and each message consumes credits based on its size and type.

This crate fully supports this feature. When built with the "async" or "multi_threaded" options, you can share SMB resources between threads, enabling concurrent access and efficient utilization of available network bandwidth. Requests can be sent and processed even while awaiting responses.

Credits are managed per connection, so you should maintain a single connection between your client and each server. If you have multiple network adapters and want to maximize bandwidth by connecting to multiple endpoints (i.e., establishing multiple connections between the same client and server on different adapters), SMB provides a solution called multi-channel.

## Practice: How should I do it?

In practice, using this crate should be straightforward when you need to parallelize operations: You need to open your resource, and simply spawn some tasks (or threads) that share a reference to that resource. In turn, you may definitely perform operations on the resource from multiple tasks or threads at the same time, and thread safety is guaranteed by the crate!

1. Open your common resource.
1. Divide your work into tasks that can be performed concurrently.
1. Spawn the tasks, sharing the resource reference.
1. Avoid locks on long-running tasks.

## Examples

### Parallel file copy

Refer to the [`crate::resource::file_util::block_copy`] function: it implements a parallel file copy operation, both in async and multi-threaded modes. The function starts a number of workers, shares the open remote file, and each task acquires a lock, chooses the next data block to read or write from the remote file, and performs the operation concurrently.
In practice, taking a look at a sniffing of a copy session that uses parallel copy, you may notice many requests getting sent over the wire before responses arrive, and even responses that arrive in a non-deterministic, out-of-sequence manner. This shows that operations are indeed being processed in parallel, making efficient use of the available bandwidth.

- In this example, as mentioned, the `File` instance is being shared between the tasks, using `Arc<File>` to allow concurrent access (`File` provides many a rich api that accepts a safe `&self` argument).
- No locking is performed at all - the only synchronization mechanism used when copying the file is an `AtomicU64`, describing the current position in the file being copied.

## Additional resources & references

- The SMB protocol documentation - MS-SMB2
  - Multi-Credit documentation resources [Credit Charge](https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-smb2/18183100-026a-46e1-87a4-46013d534b9c), [Granting Message Credits](https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-smb2/dc517c41-646d-4d0b-b7b3-25a53932181d), [Verifying the Sequence Number](https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-smb2/0326f784-0baf-45fd-9687-626859ef5a9b)
  - Multi-Channel example: [Establish Alternate Channel](https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-smb2/2e32e57a-166f-46ae-abe8-17fa3c897890)

- [Samba Wiki/SMB2 Credits](https://wiki.samba.org/index.php/SMB2_Credits)
- SNIA.org - Smb3 in samba - [Multi-Channel and beyond](https://www.snia.org/sites/default/files/SDC/2016/presentations/smb/Michael_Adam_SMB3_in_Samba_Multi-Channel_and_Beyond.pdf)
- Issue [#104](https://github.com/afiffon/smb-rs/issues/104) - "Parallelize File fetching and downloading"
