# Contributing to p99gate

Thanks for your interest! Bug reports, ideas and pull requests are all welcome.
For anything bigger than a small fix, please open an issue first so we can agree
on the approach.

## Development

```sh
cargo test                                   # unit, integration and doc tests
cargo clippy --all-targets -- -D warnings    # CI denies warnings
cargo fmt
cargo run -- demo                            # try your change end to end
```

CI runs the same checks on Linux, macOS and Windows, builds the docs with
warnings denied, and checks the minimum supported Rust version (1.85).

## Architecture

p99gate is one package with a library (`src/lib.rs`) and a thin binary
(`src/bin/p99gate/`). The library has no terminal I/O and no knowledge of the
command line. Everything a later feature needs (scenario files, run comparison,
a GitHub Action) can be built on its public API.

```text
            ┌────────────────────────── library ──────────────────────────┐
 CLI args → │ LoadConfig ─┐                                               │
            │             ├→ Engine ─ spawns ─→ Executor::execute ─┐      │
 URL, -H …→ │ RequestSpec ┘    │  (one task per request,          │      │
            │                  │   semaphore = --concurrency)     │      │
            │                  │                                  ▼      │
            │                  │      Sample { latency, send_lag, outcome }
            │                  │                                  │      │
            │                  └── Measurements ← Recorder (HDR) ←┘      │
            │                          │                                 │
            │                          ▼                                 │
            │                      RunReport ── Threshold::evaluate      │
            └──────────────────────────┼─────────────────────────────────┘
                                       ▼
                  text renderer · JSON (docs/json-schema.md) · exit code
```

| Module | Responsibility |
|---|---|
| `schedule` | `Rate` and `Schedule`: when request *i* is due. Pure and exact. |
| `engine` | The scheduler loop, concurrency limit, timeouts, sample collection, live `Progress` counters. |
| `executor` | The `Executor` trait: the protocol seam. |
| `http` | `RequestSpec` and `HttpExecutor` (hyper + rustls). |
| `outcome` | `Outcome`, `ErrorKind`, `Sample`. |
| `stats` | `Recorder`: HDR histograms and counters (crate-private). |
| `report` | `RunReport`: the stable, serialisable result. |
| `threshold` | Parsing and evaluating `--fail-if` conditions. |
| `demo` | A local server with controllable latency, used by `p99gate demo` and the tests. |
| `bin/p99gate` | Arguments (`args`), live progress (`live`), text report (`render`), exit codes (`main`). |

### Measurement invariants

These are the point of the project. Please keep them intact, and add a test if
you touch them:

1. **Send times depend only on the schedule.** Nothing about responses may
   delay or skip a scheduled request, except the concurrency limit, whose wait
   is charged to latency.
2. **Latency is measured from the intended send time.** `send_lag` records how
   late the request actually went out.
3. **Every completed request is recorded**, including failures and timeouts.
4. **Under-delivery is reported, never hidden** (`unsent`, `saturated`).

The engine tests in `src/engine.rs` run on Tokio's paused clock with fake
targets, so timing assertions are exact. `tests/http_engine.rs` repeats the key
scenarios over real sockets with tolerances for CI noise. When you change
measurement logic, check that a test would fail if the invariant broke. For
example, measuring latency from the actual send time must fail
`a_stall_is_reflected_in_the_tail_when_concurrency_is_exhausted`.

### How future features fit

- **Scenario files and OpenAPI** produce multiple `RequestSpec`s. A weighted
  executor that picks one per call implements `Executor` without changes to the
  engine. Per-endpoint stats would add a key to `Sample`.
- **Comparing runs** is a function of two `RunReport`s. The JSON schema is
  versioned so that stored reports stay readable.
- **gRPC and WebSocket** are new `Executor` implementations. `Outcome::Response`
  carries a protocol status code, and `report::Target::protocol` identifies the
  protocol.

### The JSON schema

`docs/json-schema.md` is a public contract. Adding a field is fine; renaming,
removing or changing the meaning of one requires bumping `SCHEMA_VERSION` and
updating the document in the same pull request.

## Good first contributions

- **Record the README demo GIF** with [VHS](https://github.com/charmbracelet/vhs),
  and commit the `.tape` file so it can be regenerated.
- **Shell completions** with `clap_complete`, e.g. `p99gate completions bash`.
- **Read the body from stdin** with `--body @-`.
- **More threshold metrics**, e.g. `unsent>0` or `send_lag_p99>10ms`. See
  `src/threshold.rs`.
- **HTTP/2 support**: enable hyper's `http2` feature and ALPN in the rustls
  connector, and add a flag to force a protocol version.
- **A warm-up period** whose samples are excluded from the report
  (`--warmup 5s`).
- **cargo-binstall metadata**, so `cargo binstall p99gate` downloads the
  release binaries.

## License

Unless you state otherwise, any contribution you intentionally submit for
inclusion in this project, as defined in the Apache-2.0 license, is dual licensed
under the MIT and Apache-2.0 licenses as above, without any additional terms or
conditions.
