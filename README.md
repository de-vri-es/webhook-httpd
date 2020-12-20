# http-webhookd [![tests](https://github.com/de-vri-es/http-webhookd-rs/workflows/tests/badge.svg)](https://github.com/de-vri-es/http-webhookd-rs/actions?query=workflow%3Atests)

`http-webhookd` is a simple HTTP(S) server to receive webhooks, written in Rust.

Features:
 * Run commands on POST requests based on the request URL.
 * Optionally verify the `X-Hub-Signature-256` header.
 * Optionally limit job concurrency per hook.
 * Supports TLS with OpenSSL.

Hooks are configured as a sequence of commands to execute when a POST request is made for a certain URL.
A hook can run an arbitrary number of commands, and you can configure any number of hooks for different URLs.
For each command run by a hook, you can configure if it should receive the request body as standard input.

## Scheduling
The server supports limiting the number of concurrently running jobs per hook.
When the concurrency limit is reached, jobs can be put in a first-in-first-out or last-in-first out queue.
Each hook can have a different concurrency limit, queue type and maximum queue size.

By default, a hook will run only one job concurrently, and will queue at most one job in a LIFO queue (meaning older jobs are dropped when the queue is full).
This is a good configuration for hooks that just want to update things based on the latest request,
but all parameters can be changed individually per hook.

## Example configuration
A small configuration is shown below.
For a more detailed example with comments, see [`example-config.yaml`](example-config.yaml) or run `http-webhookd --print-example-config`.

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
