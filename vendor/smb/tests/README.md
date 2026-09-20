# Integration tests

## Running the tests

The tests below are intended to test the library's integration with the SMB protocol,
by starting up a samba container and running the tests against it.

To start the container, run the following command:

```bash
docker compose up [-d]
```

Then, you can run the tests as usual, using `cargo test`.

> [!IMPORTANT]
> The tests bind to port 445 by default, so make sure it is available on your machine.
> On many windows machines, this port is already in use by the system;
> Modify docker-compose.yml to use a different port if necessary,
> and use the `SMB_RUST_TESTS_SERVER=HOST:PORT` environment variable
> to specify the new port.
> The same goes for the IP address, if necessary.
