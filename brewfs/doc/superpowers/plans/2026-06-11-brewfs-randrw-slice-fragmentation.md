# BrewFS Randrw Slice Fragmentation Follow-up Plan

## Goal

Close the BrewFS `fio-randrw` gap by reducing small-slice and small-object amplification without hiding cost in page cache or post-fio drain.

## Evidence

Diagnostic run:

```bash
cd /mnt/slayerfs/brewfs/.worktrees/perf-tune-integration
PERF_TOOLS="fio-randrw" \
PERF_FIO_DIRECT_MATRIX="0 1" \
PERF_FIO_POST_WRITE_DRAIN=true \
PERF_FIO_POST_WRITE_DRAIN_TIMEOUT_SECS=900 \
bash brewfs/docker/compose-xfstests/run_redis_perf.sh \
  --s3 \
  --writeback-throughput-profile \
  --tools "fio-randrw"
```

Artifact:

```text
brewfs/docker/compose-xfstests/artifacts/perf-run-1781197298-12549
```

Summary:

```text
fio-randrw-direct0: pass, wall=88s, post-write-drain=2s
fio-randrw-direct1: pass, wall=71s, post-write-drain=32s
```

Post-drained writer diagnostics:

```text
direct0:
  s3_put_ops=12970, s3_put_mib=5659.0, fuse_write_mib=5996.0, byte_amp=0.944
  upload_batch_ops=12966, avg_upload_batch_mib=0.462, partial_tail_ratio=0.955
  slice_create_ops=12876, slice_reuse_ops=44375
  freeze_size/max_unflushed/explicit_flush/auto=51/11128/130/1567

direct1:
  s3_put_ops=1235, s3_put_mib=4602.3, fuse_write_mib=4156.0, byte_amp=1.107
  upload_batch_ops=1023, avg_upload_batch_mib=4.047, partial_tail_ratio=0.032
  slice_create_ops=872, slice_reuse_ops=3284
  freeze_size/max_unflushed/explicit_flush/auto=7/76/95/694
```

Interpretation:

- `direct0` is dominated by sub-block upload batches, not post-write drain.
- `max_unflushed` freezes are the primary source of direct0 fragmentation: 11,128 freezes versus 51 size freezes.
- 95.5% of direct0 upload batches include a frozen partial tail, so most PUTs are small object fragments.
- `direct1` already has much larger batches and low partial-tail ratio, so any fix must protect direct1 write p99 and drain time.

## Hypothesis

`ChunkHandle::find_slice_or_create` freezes older writable slices as soon as they are more than `MAX_UNFLUSHED_SLICES` away from the newest slice. Under buffered random writeback (`direct=0`), this freezes many sub-block slices before they have a chance to absorb overwrites. Delaying that specific `max_unflushed` freeze for sub-block slices should reduce partial-tail batch count and PUT count.

## Candidate A

Change only the `max_unflushed` freeze path:

- Do not freeze a writable slice for `max_unflushed` unless its logical length is at least one block.
- Leave explicit flush, size/chunk-end freeze, auto-flush, pressure flush, and commit-age safety unchanged.
- Keep the change local and reversible; if memory or direct1 tail regresses, reject it.

Expected movement:

- `fio-randrw-direct0` `writeback_freeze_max_unflushed_ops` decreases.
- `fio-randrw-direct0` `writeback_upload_partial_tail_ops / writeback_upload_batch_ops` decreases.
- `fio-randrw-direct0` `s3_put_ops_per_gib_written` decreases.
- `fio-randrw-direct1` read/write BW does not regress by more than 5%, and write p99/p99.9 does not regress by more than 25%.

## Verification

Targeted code gates:

```bash
CARGO_TARGET_DIR=/mnt/slayerfs/brewfs/target CARGO_INCREMENTAL=0 \
  cargo test -p brewfs --lib 'vfs::io::writer::tests::test_idx_need_upload'

CARGO_TARGET_DIR=/mnt/slayerfs/brewfs/target CARGO_INCREMENTAL=0 \
  cargo test -p brewfs --lib vfs::io::writer::tests::test_dirty_breakdown_reports_slice_lifecycle_metrics

CARGO_TARGET_DIR=/mnt/slayerfs/brewfs/target CARGO_INCREMENTAL=0 \
  cargo test -p brewfs --lib 'vfs::stats::tests::'

CARGO_TARGET_DIR=/mnt/slayerfs/brewfs/target CARGO_INCREMENTAL=0 \
  cargo clippy -p brewfs --lib -- -D warnings

bash brewfs/tools/perf/test_compare_artifacts.sh
bash -n brewfs/docker/compose-xfstests/run_perf_in_container.sh brewfs/docker/compose-xfstests/run_redis_perf.sh
```

Perf gate:

```bash
PERF_TOOLS="fio-randrw" \
PERF_FIO_DIRECT_MATRIX="0 1" \
PERF_FIO_POST_WRITE_DRAIN=true \
PERF_FIO_POST_WRITE_DRAIN_TIMEOUT_SECS=900 \
bash brewfs/docker/compose-xfstests/run_redis_perf.sh \
  --s3 \
  --writeback-throughput-profile \
  --tools "fio-randrw"
```

Compare against `perf-run-1781197298-12549` with:

```bash
python3 brewfs/tools/perf/compare_artifacts.py \
  brewfs/docker/compose-xfstests/artifacts/perf-run-1781197298-12549 \
  <candidate-artifact> \
  --format markdown
```

## Candidate A Result

Artifact:

```text
brewfs/docker/compose-xfstests/artifacts/perf-run-1781198447-23355
```

Post-drained diagnostics:

```text
direct0:
  s3_put_ops=11180, s3_put_mib=6600.2, fuse_write_mib=7000.0, byte_amp=0.943
  upload_batch_ops=11174, avg_upload_batch_mib=0.626, partial_tail_ratio=0.932
  slice_create_ops=11061, slice_reuse_ops=71217
  freeze_size/max_unflushed/explicit_flush/auto=59/120/378/10504

direct1:
  s3_put_ops=1135, s3_put_mib=4239.4, fuse_write_mib=3772.0, byte_amp=1.124
  upload_batch_ops=916, avg_upload_batch_mib=4.092, partial_tail_ratio=0.028
  slice_create_ops=768, slice_reuse_ops=3004
  freeze_size/max_unflushed/explicit_flush/auto=8/81/86/593
```

Movement versus baseline:

- `direct0` max-unflushed freezes dropped from 11,128 to 120 (-98.9%).
- `direct0` S3 PUT count dropped from 12,970 to 11,180 while fio wrote 16.7% more data.
- `direct0` S3 PUTs per GiB written improved from about 2,215 to 1,636 (-26.1%).
- `direct0` average upload batch size improved from 0.462 MiB to 0.626 MiB (+35.5%).
- `direct0` write bandwidth improved from 84.5 MiB/s to 115.2 MiB/s (+36.2%).
- `direct1` write bandwidth moved from 59.5 MiB/s to 56.7 MiB/s (-4.7%), drain improved from 32s to 27s, and write p99 improved from 10.4s to 7.9s.
- `direct1` read bandwidth was -5.6%, just outside the planned 5% guard band, so run a direct1-only guard before accepting the code.

Interpretation:

- Candidate A is directionally effective for buffered randrw: it converts premature `max_unflushed` partial-tail freezes into slice reuse and larger uploads.
- Remaining `direct0` partial-tail ratio is still high at 93.2%, so the next bottleneck is auto/explicit flush of sub-block tails rather than `max_unflushed`.
- Direct I/O must remain protected in follow-up work; the current result is close enough to the guard band to require a direct1 repeat before commit.

Direct1 guard artifact:

```text
brewfs/docker/compose-xfstests/artifacts/perf-run-1781199012-6131
```

Guard result versus baseline:

```text
direct1:
  read_bw_mib_s=190.4 vs 129.4 (+47.1%)
  write_bw_mib_s=85.9 vs 59.5 (+44.5%)
  post_write_drain_s=18 vs 32 (-43.8%)
  s3_put_ops_per_gib_written=258.6 vs 304.3 (-15.0%)
  avg_upload_batch_mib=4.091 vs 4.047 (+1.1%)
  partial_tail_ratio=0.032 vs 0.032
```

Decision:

- Accept Candidate A.
- It improves buffered random write fragmentation and does not show a stable direct I/O regression.
- Next optimization target: reduce `direct0` auto/explicit flush partial-tail uploads while preserving direct1 batch shape and drain behavior.

## Candidate B Result

Hypothesis:

- Make background `flush_once()` non-strict so it does not seal sub-block tails.
- Delay cached-writeback sub-block `auto_flush` until the 5s safety age or memory pressure.
- Keep strict `flush()/fsync()/truncate()/close` semantics unchanged.

Artifact:

```text
brewfs/docker/compose-xfstests/artifacts/perf-run-1781200659-21772
```

Post-drained diagnostics versus Candidate A:

```text
direct0:
  s3_put_ops=8473 vs 11180
  fuse_write_mib=6484.0 vs 7000.0
  s3_put_ops_per_gib_written=1338.1 vs 1635.5
  avg_upload_batch_mib=0.711 vs 0.626
  partial_tail_ratio=0.903 vs 0.932
  freeze_size/max_unflushed/explicit_flush/auto=58/136/372/7630
  write_bw_mib_s=89.8 vs 115.2
  post_write_drain_s=4 vs 5

direct1:
  s3_put_ops_per_gib_written=308.0 vs 308.1
  avg_upload_batch_mib=4.151 vs 4.092
  partial_tail_ratio=0.015 vs 0.028
  write_bw_mib_s=56.0 vs 56.7
  post_write_drain_s=46 vs 27
```

Decision:

- Reject Candidate B.
- It reduced PUT/object amplification, but direct0 write bandwidth regressed by about 22% and direct1 post-write drain regressed by about 70%.
- The rollback keeps Candidate A as the current accepted code.

Interpretation:

- Delaying cached sub-block sealing too broadly shifts cost into dirty/backpressure queues: `writeback_hard_wait_ms` grew from about 9.3M to 28.4M in direct0.
- The next attempt should avoid holding dirty data longer globally. Prefer either per-reason metrics first, or a narrower change that reduces object count without increasing recent-pending hard waits.
