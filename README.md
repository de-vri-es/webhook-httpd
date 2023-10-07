# webhook-httpd [![tests](https://github.com/de-vri-es/webhook-httpd/workflows/tests/badge.svg)](https://github.com/de-vri-es/webhook-httpd/actions?query=workflow%3Atests)

`webhook-httpd` is a simple HTTP(S) server to receive webhooks, written in Rust.

Features:
 * Run commands on POST requests based on the request URL.
 * Optionally verify the `X-Hub-Signature-256` header.
 * Optionally limit job concurrency per hook.
 * Supports TLS with OpenSSL.

Hooks are configured as a sequence of commands to execute when a POST request is made for a certain URL.
A hook can run an arbitrary number of commands, and you can configure any number of hooks for different URLs.
For each command run by a hook, you can configure if it should receive the request body on standard input.

## Scheduling
The server supports limiting the number of concurrently running jobs per hook.
When the concurrency limit is reached, jobs can be put in a first-in-first-out or last-in-first out queue.
Each hook can have a different concurrency limit, queue type and maximum queue size.

By default, a hook will run only one job concurrently, and will queue at most one job in a LIFO queue (meaning older jobs are dropped when the queue is full).
This is a good configuration for hooks that just want to update things based on the latest request,
but all parameters can be changed individually per hook.

## Command environment
Each command is executed with some environment variables set.
The variables provide some information about the HTTP request that was made:
* `URL_PATH`: The path portion of the request URL.
* `URL_QUERY`: The query portion of the request URL.
* `REMOTE_ADDR`: The IP address of the remote peer.
* `REMOTE_PORT`: The port number of the remote peer.

You can also configure a command to receive the request body on its standard input.
In that case, some additional environment variables are set:
* `CONTENT_TYPE`: The value of the `Content-Type` header.
* `CONTENT_LENGTH`: The size of the request body in bytes.

## Example configuration
A small configuration is shown below.
For a more detailed example with comments, see [`example-config.yaml`](example-config.yaml) or run `webhook-httpd --print-example-config`.

```yaml
port: 8091
tls:
  private-key: /etc/letsencrypt/live/example.com/privkey.pem
  certificate-chain: /etc/letsencrypt/live/example.com/fullchain.pem

hooks:
  - url: "/make-release-tarball"
    commands:
      - cmd: ["make-release-tarball"]
        stdin: request-body
    working-dir: "/path/to/repository/"
    max-concurrent: 1
    queue-size: unlimited
    queue-type: fifo
    secret: "some-randomly-generated-secret"

  - url: "/update-daemon-config"
    commands:
      - cmd: ["git", "fetch"]
      - cmd: ["git", "reset", "--hard", "origin/main"]
      - cmd: ["systemctl", "reload", "my-little-service"]
    working-dir: "/etc/my-little-service/"
    secret: "some-randomly-generated-secret"
```

## Features
The crate has one optional feature: `static-openssl`.
When the feature is enabled, `openssl` is linked statically against a locally compiled OpenSSL.
This can be used to create a binary with a minimal set of runtime dependencies,
and it can make compilation easier on systems with no recent version of OpenSSL readily available.

For more information on how to build with a locally installed version of OpenSSL see:
https://docs.rs/openssl/latest/openssl/#building

## Examples
The `multipart-stdin` example shows how to process `multipart/form-data` from stdin and how to pass additional environment variables to your hooks from the config file.

Build the example
```sh
 cargo build --example multipart-stdin --features static-openssl
```

Add the hook:
```yaml:
  - url: "/multipart-stdin"
    commands:
      - cmd: [""target/debug/examples/multipart-stdin"]
        stdin: request-body
    environment:
        OUTPUT_FOLDER: uploads
        PREFIX_TIMESTAMP: 1
```

Run the server:
```sh
cargo run --features static-openssl -- --config example-config.yaml
```

You can test the endpoint using `curl` with the `-F` option:
```sh
curl -X POST -F "key1=value1" -F "key2=value2" -F "file=@Cargo.toml"   http://localhost:8091/multipart-stdin
```
