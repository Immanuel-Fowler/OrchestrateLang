# Benchmarks

## landline_latency

Measures round-trip call latency — from calling a serverlet handler until its reply
arrives — for the same echo handlers in three kinds of serverlet:

| Kind | Where the handler runs |
|---|---|
| `in-process` | An ordinary serverlet: a channel hop inside the program (the baseline) |
| `secret` | A Rust child process, over stdin/stdout |
| `python` | A Python landline, over stdin/stdout |

Payloads are an `int`, a 1,024-character `string`, and a 1,000-element `int[]`. Each case
makes 200 warm-up calls, then 2,000 measured calls, one after another.

```sh
cargo build --release
ORCHESTRATE=target/release/orchestrate python3 benchmarks/landline_latency/run.py
```

`run.py` builds the program in release mode, runs it, and prints p50, p90, p99, and max
in microseconds. Python 3.10+ is required.

Reading the results:

- Numbers depend on the machine, OS scheduler, and load. Compare cases on one machine.
- For per-tick work, the tail (p99, max) matters more than the median. Use it to choose a
  landline `budget`.
- Calls run one at a time; concurrent callers queue behind each other on one serverlet.
- The benchmark is not run in CI, because shared runners are too noisy for timing. CI
  only checks that `main.orch` still typechecks.

### Sample results

One run on an Apple Silicon Mac (macOS, arm64, Python 3.12.7), 2026-09-14. Your numbers
will differ.

| Serverlet | Payload | p50 | p90 | p99 | max |
|---|---|---:|---:|---:|---:|
| in-process | int | 8 | 12 | 22 | 36 |
| in-process | string-1KB | 9 | 12 | 23 | 93 |
| in-process | int[1000] | 7 | 10 | 22 | 51 |
| secret | int | 17 | 23 | 41 | 79 |
| secret | string-1KB | 17 | 22 | 34 | 41 |
| secret | int[1000] | 23 | 29 | 41 | 110 |
| python | int | 24 | 27 | 38 | 51 |
| python | string-1KB | 26 | 28 | 41 | 60 |
| python | int[1000] | 573 | 641 | 1606 | 3466 |

What this run shows:

- The pipe itself is cheap: a Rust child adds roughly 10–20 µs over an in-process call.
- Python handlers with small payloads cost about 25 µs at the median, 40 µs at p99.
- Large Python arrays are dominated by the SDK decoding and encoding each element
  individually. That is the next optimization target, not the frame format.
