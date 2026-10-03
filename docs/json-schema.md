# JSON output schema

`p99gate run --output json` (and `p99gate demo --output json`) writes one JSON
object describing the whole run. This document is the contract for that object.

## Versioning

`schema_version` is currently **1**.

- **Adding** a field is not a breaking change and does not bump the version.
  Consumers should ignore fields they do not recognise.
- **Removing, renaming, or changing the meaning or unit** of a field bumps the
  version.

## Units

Units are part of each field name:

| Suffix   | Meaning                         |
|----------|---------------------------------|
| `_us`    | integer microseconds            |
| `_s`     | seconds, as a float             |
| `_per_s` | a rate per second, as a float   |

Ratios such as `error_rate` are floats from `0.0` to `1.0`.

## Latency definition

All latencies are measured from the **intended** send time of a request, as
set by the constant-rate schedule, to the moment its response (or error)
completed. If the generator falls behind schedule, for example because every
`--concurrency` slot is waiting on a slow server, that wait is part of the
latency. This corrects for *coordinated omission*. `send_lag` records how late
requests actually went out, so you can tell how much of the latency was spent
waiting to be sent.

Every completed request contributes to `latency`, including failed ones. A
timed-out request contributes the time until it timed out.

## Fields

| Field | Type | Description |
|---|---|---|
| `schema_version` | integer | Always `1` for this version of the schema. |
| `p99gate_version` | string | Version of p99gate that produced the report. |
| `target.protocol` | string | `"http"`. |
| `target.method` | string | HTTP method, e.g. `"GET"`. |
| `target.url` | string | Request URL. |
| `target.proxy` | string or null | The proxy requests went through, without credentials, e.g. `"http://proxy:3128"`. `null` for direct connections. Proxy latency is part of every measurement. |
| `config.rate_per_s` | float | Target request rate. |
| `config.duration_s` | float | Configured sending duration. |
| `config.concurrency` | integer | Maximum requests in flight. |
| `config.timeout_s` | float | Per-request timeout, measured from the actual send time. |
| `run.started_at` | string | RFC 3339 UTC timestamp, millisecond precision. |
| `run.send_window_s` | float | Time from start until sending stopped. |
| `run.elapsed_s` | float | Time from start until the last response arrived. |
| `run.interrupted` | bool | `true` if the run was stopped early (Ctrl-C). |
| `requests.scheduled` | integer | Requests the schedule called for over the full duration. |
| `requests.sent` | integer | Requests sent. |
| `requests.unsent` | integer | Requests that were due but never sent because the generator fell behind. |
| `requests.completed` | integer | Requests that finished; `succeeded + failed`. |
| `requests.succeeded` | integer | Completed with a status below 400. |
| `requests.failed` | integer | Completed with a transport error or a status of 400 or above. |
| `throughput.target_per_s` | float | Same as `config.rate_per_s`. |
| `throughput.sent_per_s` | float | `requests.sent / run.send_window_s`. |
| `throughput.completed_per_s` | float | `requests.completed / run.send_window_s`. Responses arriving during the drain after sending stops are included, so a run in which every request completes reports the same value as `sent_per_s`. |
| `error_rate` | float | `requests.failed / requests.completed` (`0.0` when nothing completed). |
| `latency.min_us` … `latency.max_us` | integer | `min`, `mean`, `p50`, `p75`, `p90`, `p95`, `p99`, `p999` and `max`, each suffixed `_us`. |
| `latency.histogram` | array | Buckets `{ "le_us": integer, "count": integer }` on a 1-2-5 scale. Each counts samples greater than the previous bucket's `le_us` and at most its own. Empty leading buckets are omitted. |
| `send_lag.*_us` | integer | Same percentile fields as `latency`, without the histogram. |
| `status_codes` | object | Status code (as a string key) to count. |
| `errors` | object | Transport error kind to count. Kinds: `timeout`, `connect`, `io`, `other`. |
| `saturated` | bool | `true` if `requests.unsent > 0` or `send_lag.p99_us` exceeds 50 ms: the target rate was not sustained. |
| `thresholds` | array | One entry per `--fail-if`, in order: `{ "expression": string, "observed": float, "unit": string, "violated": bool }`. `unit` is `"us"` for latency metrics, `"ratio"` for `error_rate` and `"per_s"` for `rps`. `violated: true` means the condition held and the run failed. |

Percentiles come from an HDR histogram with 3 significant digits, so each value
is accurate to within 0.1%.
