# Reference Performance Acceptance

Status: **PASS — Section 7 project-check target met after scoped query and
transaction correction**

Measured on 2026-07-19 on the Task 16 branch based on source commit `1adceef`.
The exact SPEC 23.8 dataset and all requested dimensions were exercised. The
original transition-heavy failure is retained below as an operational
observation.

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
| Ready items | 20,000 | 20,000 scheduled time triggers at the fixed clock |
| Events | 20,000,000 | 20,000,000 |
| Mean events per item | 20 | 20.0 |

All 100,000 active items begin in `scheduled`; the first 20,000 have
`next_fire_at` equal to the fixed check clock and the other 80,000 are in the
future. This preserves transition work in the measured check rather than
pre-seeding due state.

Item generation took 25.939 s and event generation took 172.395 s.

## Results

Latency values are milliseconds over 40 samples. p95 uses the nearest-rank
method: sort all samples and select rank `ceil(0.95 × 40) = 38`.

| Dimension | Median | p95 | Max | Result |
|---|---:|---:|---:|---|
| Project list, 100 rows | 418.053 | 496.584 | 7,563.925 | measured |
| Project/task list, 100 rows | 45.226 | 49.724 | 51.918 | measured |
| Full indexed project check, 50 results | 69.575 | **101.811** | 123.233 | **PASS: <250 ms p95** |
| Due-candidate query, 500 rows | 608.603 | 649.104 | 748.688 | measured |
| History page, 10 events | 0.476 | 1.290 | 3.226 | measured |
| Write under four concurrent readers | 10.509 | 15.112 | 23.373 | measured |

The four concurrent readers completed 708 indexed count queries during the 40
write samples.

### Retained transition-heavy baseline

Before the correction, the same all-scheduled-ready fixture produced a full
project-check median of 248.183 ms, p95 of **728.547 ms**, and maximum of
751.389 ms. That result failed the target. Its cause was:

- selection loaded all 10,000 active rows per project, including 8,000 future
  time triggers, and used a temporary B-tree for a redundant SQL sort; and
- each 50-item ready page used 50 serial `BEGIN IMMEDIATE` transactions and 50
  `synchronous=FULL` commits.

The corrected path uses four disjoint `UNION ALL` eligibility branches over
existing indexes, direct comparisons over canonical UTC timestamps, no
repository sort, and one transaction for the selected page. It retains
per-item version checks, CAS updates, immutable events, skip-on-conflict
behavior, and the later source-defined ready-result ordering.

The actual `Scheduler` processed all 20,000 ready candidates through its
production candidate parsing, state transition, event, lease, and
`synchronous=FULL` write paths:

| Scheduler measure | Result |
|---|---:|
| Selected | 20,000 |
| Applied | 20,000 |
| Failures/conflicts | 0 / 0 |
| Elapsed | 190.595 s |
| Throughput | 104.934 evaluations/s |

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
- Project checks include normal scheduled-to-due transitions. The first sample
  for each project performs one 50-item transaction and one durable commit.
  Before scheduler measurement, the harness removes those measurement-only
  events and restores the exact 20,000 ready candidates.
- The database is larger than memory cache and the run includes normal warm-up
  effects. No samples were discarded, including the 7.564 s first project-list
  outlier.
- The final acceptance figures come from one exact run on the documented host.
  The retained baseline and an intermediate steady-state diagnostic run are
  supporting evidence, not substitutions for the final all-scheduled run.
