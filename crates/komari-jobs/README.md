# komari-jobs

Rust port of the Go scheduled maintenance in `cmd/server.go` and
`database/records`:

* `records.DeleteRecordBefore(now - record_preserve_time)` with batched deletes
  across `records`, `records_long_term`, `gpu_records`,
  `gpu_records_long_term`.
* `records.CompactRecord`: live rows older than four hours are grouped per
  client (and GPU device name/index) into 15-minute buckets in the long-term
  tables, then raw rows older than `now - 5h` are removed, SQLite is
  checkpointed (`PRAGMA wal_checkpoint(TRUNCATE)`) and vacuumed. A VACUUM
  failure is logged, not propagated, matching Go.
* `run_once` / `spawn(db, interval, cancel)` provide a one-shot pass and a
  cancellable background loop. The crate never starts a loop implicitly.

## Compatibility notes

* Most metrics use the 70th percentile; `net_in`/`net_out` use the 20th
  percentile, exactly like Go. Integer aggregates truncate the interpolated
  float (for example `3099.999…` becomes `3099`).
* The live-window boundary is absolute (`now - 4h`); 15-minute buckets follow
  the wall clock. Time zones whose UTC offset is not a multiple of 15 minutes
  could bucket differently from Go's `time.Truncate`, which is not the case for
  the zones Komari targets.
* Upserts check `(client, time)` and `(client, device_index, time)`, so
  repeated passes are idempotent and never duplicate long-term rows.
* `JobOptions::preserve_hours` and `batch_size` are injectable for tests; the
  clock is passed explicitly as `DateTime<Utc>`. `CompactionOptions::default`
  and `JobOptions::default` both enable VACUUM.
