# Reference Performance Acceptance

Status: **FAIL — Section 7 project-check target not met**

Measured on 2026-07-19 at source commit `1adceef`. The exact SPEC 23.8
dataset and all requested dimensions were exercised. The result is not an
acceptance of the measured deviation.

## Reproduce

```sh
REMINDI_RUN_REFERENCE_PERFORMANCE=1 \
REMINDI_PERF_DIR=target \
cargo +1.97.1 test --release --test performance --locked -- \
  --ignored --nocapture --test-threads=1
```

The test uses an isolated temporary directory below `REMINDI_PERF_DIR` and
removes the multi-gigabyte database when it exits. It is ignored during normal
test runs because it creates 21 million rows.

## Reference host

| Component | Measured value |
|---|---|
| Environment | KVM virtual machine |
| CPU | Intel Xeon E5-2699 v3 at 2.30 GHz |
| CPU allocation | 8 cores, 1 thread per core |
| Memory | 31 GiB |
| Storage | 512 GiB non-rotational QEMU virtual disk |
| OS | Linux 7.0.0-27-generic, x86_64 |
| Rust | `rustc 1.97.1 (8bab26f4f 2026-07-14)` |
| Cargo | `cargo 1.97.1 (c980f4866 2026-06-30)` |
| SQLite | 3.51.3 through SQLx 0.9.0 |
| Database mode | WAL, `synchronous=FULL`, foreign keys enabled |

The run was not CPU-pinned and the host caches were not dropped. Results are
therefore representative of this shared home-lab VM, not a claim about other
hardware.

## Dataset verification

The harness bulk-loads the production schema, then verifies the reference
dataset by SQL before measuring:

| Check | Required | Measured |
|---|---:|---:|
| Remindi items | 1,000,000 | 1,000,000 |
| Active items | 100,000 (10%) | 100,000 (10%) |
| Projects | 10 | 10 |
| Ready items | 20,000 | 20,000 |
| Events | 20,000,000 | 20,000,000 |
| Mean events per item | 20 | 20.0 |

Item generation took 27.900 s and event generation took 180.097 s.

## Results

Latency values are milliseconds over 40 samples. p95 uses the nearest-rank
method: sort all samples and select rank `ceil(0.95 × 40) = 38`.

| Dimension | Median | p95 | Max | Result |
|---|---:|---:|---:|---|
| Project list, 100 rows | 361.025 | 454.585 | 5,599.056 | measured |
| Project/task list, 100 rows | 43.064 | 55.619 | 63.253 | measured |
| Full indexed project check, 50 results | 248.183 | **728.547** | 751.389 | **FAIL: must be <250 ms p95** |
| Due-candidate query, 500 rows | 557.096 | 691.212 | 760.768 | measured |
| History page, 10 events | 0.562 | 0.984 | 6.963 | measured |
| Write under four concurrent readers | 8.767 | 14.430 | 28.133 | measured |

The four concurrent readers completed 679 indexed count queries during the 40
write samples.

The actual `Scheduler` processed all 20,000 ready candidates through its
production candidate parsing, state transition, event, lease, and
`synchronous=FULL` write paths:

| Scheduler measure | Result |
|---|---:|
| Selected | 20,000 |
| Applied | 20,000 |
| Failures/conflicts | 0 / 0 |
| Elapsed | 190.709 s |
| Throughput | 104.872 evaluations/s |

## Database and WAL growth

| Point | Database | WAL | Combined |
|---|---:|---:|---:|
| Migrated empty database | 4,096 B | 144,232 B | 148,328 B |
| After 1,000,000 items | 495,865,856 B | 7,304,792 B | 503,170,648 B |
| Exact reference dataset | 5,743,263,744 B | 53,234,552 B | 5,796,498,296 B |
| After all measurements | 5,751,648,256 B | 53,234,552 B | 5,804,882,808 B |

## Method and limitations

- Dataset setup uses chunked bulk SQL only to make the exact 21-million-row
  fixture practical. It does not change schema, indexes, pragmas, or product
  behavior.
- Project/task list, project check, history, writes, and scheduler throughput
  call production Rust services. The due-candidate latency measurement mirrors
  the private production repository SQL exactly; the scheduler measurement
  separately exercises that production repository path end to end.
- Project checks include normal state-transition writes when a scheduled item
  first becomes due. Before scheduler measurement, the harness removes those
  measurement-only events and restores the exact 20,000 ready candidates.
- The database is larger than memory cache and the run includes normal warm-up
  effects. No samples were discarded, including the 5.599 s first project-list
  outlier.
- One exact run was performed on the documented host. The failing target needs
  query-path investigation and a repeated exact run before version 1
  performance acceptance can pass.
