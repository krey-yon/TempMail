# SMTP p95 under 1000 concurrent connections

Date of this run: 2026-09-02.

This note is what we actually measured after making inbound SMTP concurrent, plus the programming ideas that made the number real. It is not a marketing page. Client RTT and server work are different clocks. Only the server clock is the founder line.

## What was wrong before

The SMTP listener accepted one TCP socket, then waited until that session finished (`LocalSet::run_until`) before accepting the next one. Tokio ran as `current_thread`.

That means:

- N live sessions was 1.
- Any "p95" was a single-client number.
- A claim of 1000 concurrent connections was false, even if one mail looked fast.

`DATA` to `250 Ok` was already in memory. Postgres runs later on `QUIT`. The protocol path was not the bottleneck. The accept loop was.

## What we changed

1. `tokio::spawn` one task per accepted connection.
2. Multi-thread Tokio runtime.
3. `TCP_NODELAY` on each socket.
4. In-process atomics for inflight count, greeting latency, and DATA-accept latency.
5. JSON snapshot on `SMTP_METRICS_PORT` (default 9100), inside the SMTP process. HTTP `/api/stats` does not see these numbers. HTTP and SMTP are separate processes.

Greeting timer: `accept()` to a successful `220` write.

DATA-accept timer: after the terminator chunk (`.`) is read, until `250 Ok` is written. Idle wait and Postgres are not included.

## How this run was done

- Branch with the concurrent server: `feat/smtp-concurrent-p95`
- Machine: macOS arm64, 10 cores, 16 GB RAM, localhost (not the e2-standard-2 VM)
- Build: `cargo run -p smtp --bin smtp` (debug)
- Bind: `SMTP_PORT=2525`, `SMTP_METRICS_PORT=9100`
- Load: 1000 clients opened at once, each waited for `220`, all stayed open, then all ran `HELO` / `MAIL FROM` / `RCPT TO` / `DATA` / `.`
- Sockets were closed after `250`. No `QUIT`, so this run did not persist 1000 rows.

Debug builds are slower than `--release`. Localhost has almost no network RTT. Quote the server snapshot for the Rust claim. Quote a same-region VM run when you talk about e2-standard-2.

## Measured values (this machine, this run)

| Clock | Metric | Value |
|---|---|---|
| Load | Concurrent live sessions | **1000** |
| Load | Wall time to get 1000 `220` replies | 44.7 ms |
| Server | `inflight_connections` while held | **1000** |
| Server | `greeting_count` | 1000 |
| Server | `greeting_p95_ms` | **0.0** |
| Server | `data_accept_count` | 1000 |
| Server | `data_accept_p95_ms` | **0.0** |
| Client | greeting p50 | 29.68 ms |
| Client | greeting p95 | 40.52 ms |
| Client | greeting max | 41.76 ms |
| Client | DATA-accept p50 | 10.41 ms |
| Client | DATA-accept p95 | **15.20 ms** |
| Client | DATA-accept max | 15.76 ms |

Server JSON after DATA:

```json
{"inflight_connections": 1000, "greeting_count": 1000, "greeting_p95_ms": 0.0, "data_accept_count": 1000, "data_accept_p95_ms": 0.0}
```

**How to read this.** Server p95 of `0.0` ms means the work finished in under 1 ms. The histogram uses 1 ms buckets, so sub-millisecond samples land in bucket 0. That is the number for "no GC pauses, accept and `250` are cheap."

Client greeting p95 of 40.52 ms is the 1000-way burst sitting in the accept queue. The last sockets wait until the process `accept()`s them. That is not SMTP parse time and not a Rust vs GC story. Do not quote 40 ms as the server greeting.

Client DATA-accept p95 of 15.20 ms is still under 50 ms with all 1000 sessions already live. That matches the DATA-accept half of the pitch.

### What you can say

On this run, the process held **1000 concurrent inbound SMTP connections**. Server p95 greeting was **under 1 ms**. Server p95 DATA-accept was **under 1 ms**. Client p95 DATA-accept was **15.20 ms**. There is no garbage collector on this path.

Repeat the same harness on the e2-standard-2 (2 vCPU, 8 GB) from a VM in the same region, with `ulimit -n 4096` and `--release`, before you put those VM specs in a pitch.

## Programming and architecture concepts

### 1. Serial accept vs concurrent sessions

A listen socket can have many completed TCPs in the kernel backlog. If userspace handles them one at a time, only one session is in `EHLO` / `DATA`. The rest wait.

`tokio::spawn` makes each session an independent task. The accept loop goes back to `accept()` immediately. Inflight can reach 1000. That is the whole performance change.

### 2. Tasks are not threads

A Tokio task is a state machine the runtime polls. It is not one OS thread per connection. SMTP is mostly idle (waiting on `read`). Tasks are the right unit. A thread per connection would waste stacks and scheduler time.

`current_thread` plus `LocalSet` was a second serial trap. `!Send` spans (`span.enter()` across `.await`) forced that shape. The fix was a `Send` span (`tracing::instrument`) so the session future can move to the multi-thread runtime.

### 3. Measure the hot path, not the database

Latency claims need a timer around the work you claim. Greeting is bytes on the socket. DATA-accept is parse plus `250`. `QUIT` writes Postgres. If you time `QUIT`, you are timing the database pool (size 16), not Rust.

Two clocks:

- **Server clock:** `Instant` inside the process. This is the founder metric.
- **Client clock:** connect to `220`, or `.` to `250`. Includes accept queue, syscall, and RTT.

### 4. p95 is a distribution, not a percentage score

p95 is a time. "p95 greeting is 0 ms" means 95% of samples were faster than that time. It does not mean "88% good."

We store counts in 64 buckets of 1 ms, plus overflow. At read time we walk the buckets until the cumulative count hits 95% of samples. Concurrent `record()` uses `AtomicU64` with relaxed ordering. No mutex on the record path.

### 5. Shared state without a lock

1000 tasks update the same metrics object. The shared type is `Arc<SmtpMetrics>`. Mutation is atomics. We did not clone a `Vec` of samples under a `Mutex`.

Inflight is an RAII guard (`InflightGuard`). `Drop` decrements even on timeout or error. The count is tied to session lifetime, not to a matching `inc` / `dec` you might forget.

### 6. Make illegal states unrepresentable

DATA-accept is `SmtpReplyEvent::DataAccepted`, not a boolean on the SMTP state machine. The server matches the event after it writes the bytes. You cannot "forget" to tag `250` without the compiler seeing the enum.

The session itself was already a state machine (`CurrentStates`: Initial, Greeted, AwaitingRecipient, AwaitingData, DataReceived).

### 7. Process boundary

SMTP and HTTP do not share memory. Putting these counters on `GET /api/stats` would mean Postgres, a socket, or a lie. Metrics live next to the sockets they describe.

### 8. Backpressure and honesty at N=1000

`ulimit -n` (file descriptors) is a real cap. Default 1024 is too close to 1000 sockets plus Postgres plus logs. Raise it on the VM.

A thundering herd of 1000 `connect()`s at T=0 will show a slow *client* greeting p95. That is the accept queue on 2 (or 10) CPUs. The pitch is 1000 **live** connections, then DATA-accept, plus server-side greeting on each new session.

## Files

| Path | Role |
|---|---|
| `smtp/src/lib.rs` | Accept loop, spawn, greeting `Instant` |
| `smtp/src/main.rs` | Multi-thread `#[tokio::main]` |
| `smtp/src/server.rs` | `220` / `250` timers, inflight guard |
| `smtp/src/types.rs` | `SmtpReply`, `SmtpReplyEvent` |
| `smtp/src/metrics.rs` | Histogram, snapshot, port 9100 |
| `smtp/src/bin/smtp-bench.rs` | Rerun harness (`QUIT` persists mail) |

## Rerun

From a live smtp process:

```bash
ulimit -n 4096
SMTP_PORT=2525 SMTP_METRICS_PORT=9100 cargo run -p smtp --bin smtp --release
cargo run -p smtp --bin smtp-bench --release -- --port 2525 --connections 1000 --domain xelio.me
curl -s http://127.0.0.1:9100/
```

`smtp-bench` sends `QUIT` after DATA and will write mail rows. The numbers in this file used a hold-open probe that closed after `250` instead.
