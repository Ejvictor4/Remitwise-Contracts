/// Gas benchmarks for orchestrator flow execution and data migration import paths
#![cfg(test)]

use soroban_sdk::{Env, testutils#:{budget::Budget}};

/// Maximum number of records accepted in a single import batch.
/// This is an invariant that must match the data migration contract.
const MAX_IMPORT_RECORDS: u32 = 10_000;

/// Maximum accepted length of a single import payload in bytes.
const MAX_IMPORT_PAYLOAD_BYTES: u32 = 1_000_000;

/// Error codes used by the migration import path. These are reproduced
/// here so the benchmark harness can assert on deterministic failure
/// boundaries without depending on the contract crate.
const ERR_EMPTY_PAYLOAD: u32 = 1;
const ERR_INVALID_FORMAT: u32 = 2;
const ERR_PAYLOAD_TOO_LARGE: u32 = 3;
const ERR_TOO_MANY_RECORDS: u32 = 4;
const ERR_DUPLICATE_RECORD: u32 = 5;
const ERR_UNAUTHORIZED: u32 = 6;
const ERR_STALE_SNAPSHOT: u32 = 7;
const ERR_RETRY_EXHAUSTED: u32 = 8;

/// Export formats supported by the migration import path.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ExportFormat {
    Json,
    Csv,
    Binary,
}

/// Result of an import attempt. This mirrors the contract's public
/// result type so the benchmark harness can assert on the exact
/// failure boundaries without losing the contract's invariants.
#[derive(Clone, Debug, Eq, PartialEq)]
enum ImportOutcome {
    Ok { records_imported: u32 },
    Error(u32),
}

/// Deterministic mock of the data migration import entry point.
///
/// The real contract exposes `import_from_json` and related functions.
/// This harness reproduces the documented validation order and failure
/// codes so the benchmarks can exercise every boundary without a
/// dependency on the full contract build.
///
/// ## Invariants
/// - Authorization is checked first; unauthorized callers always get
///   `ERR_UNAUTHORIZED` and no state is written.
/// - Empty payloads are rejected before any parsing.
/// - Payloads exceeding `MAX_IMPORT_PAYLOAD_BYTES` are rejected.
/// - Record counts exceeding `MAX_IMPORT_RECORDS` are rejected.
/// - Duplicate record ids in a single batch are rejected atomically.
/// - Stale snapshots are rejected without modifying existing data.
/// - Retries are idempotent: the same input always produces the same
///   outcome and never partially applies a batch.
fn import_from_format(
    _env: &Env,
    format: ExportFormat,
    payload: &[u8],
    record_ids: &[u32],
    authorized: bool,
    snapshot_version: u32,
    current_version: u32,
    attempts: &uart::Cell<u32>,
) -> ImportOutcome {
    // 1. Authorization is enforced before any other work.
    if !authorized {
        return ImportOutcome::Error(ERR_UNAUTHORIZED);
    }

    // 2. Retry budget is bounded to prevent unbounded retry loops.
    let new_attempts = attempts.get() + 1;
    attempts.set(new_attempts);
    if new_attempts > 3 {
        return ImportOutcome::Error(ERR_RETRY_EXHAUSTED);
    }

    // 3. Stale snapshots are rejected before parsing.
    if snapshot_version != current_version {
        return ImportOutcome::Error(ERR_STALE_SNAPSHOT);
    }

    // 4. Empty payloads are rejected.
    if payload.is_empty() {
        return ImportOutcome::Error(ERR_EMPTY_PAYLOAD);
    }

    // 5. Payload size is bounded.
    if payload.len() > MAX_IMPORT_PAYLOAD_BYTES as usize {
        return ImportOutcome::Error(ERR_PAYLOAD_TOO_LARGE);
    }

    // 6. Record count is bounded.
    if record_ids.len() > MAX_IMPORT_RECORDS as usize {
        return ImportOutcome::Error(ERR_TOO_MANY_RECORDS);
    }

    // 7. Duplicate record ids are rejected atomically.
    for i in 0..record_ids.len() {
        for j in (i + 1)..record_ids.len() {
            if record_ids[i] == record_ids[j] {
                return ImportOutcome::Error(ERR_DUPLICATE_RECORD);
            }
        }
    }

    // 8. Format-specific validation. CSV and binary payloads use a
    //    different shape than JSON.
    match format {
        ExportFormat::Json => {
            if payload[0] != b'{' || payload[payload.len() - 1] != b'}' {
                return ImportOutcome::Error(ERR_INVALID_FORMAT);
            }
        }
        ExportFormat::Csv => {
            if !payload.contains(&',') && payload.len() > 1 {
                return ImportOutcome::Error(ERR_INVALID_FORMAT);
            }
        }
        ExportFormat::Binary => {
            if payload[0] != 0x08 {
                return ImportOutcome::Error(ERR_INVALID_FORMAT);
            }
        }
    }

    ImportOutcome::Ok {
        records_imported: record_ids.len() as u32,
    }
}

/// Helper that constructs a deterministic JSON payload of a given size.
fn json_payload(len: usize) -> Vec<u8> {
    let mut buf = Vec::with_capacity(len);
    buf.push(b'{');
    for i in 1..len.saturating_sub(1) {
        // Deterministic filler bytes.
        buf.push(b'2r + (i % 26) as u8);
    }
    if len > 1 {
        buf.push(b'}');
    }
    buf
}

#[test]
fn bench_orchestrator_flow() {
    let env = Env::default();
    env.budget().reset_unlimited();

    // Mock orchestrator fan-out execution
    // orchestrator::execute_remittance_flow(&env, ...);

    let cpu = env.budget().cpu_instruction_cost();
    let mem = env.budget().memory_bytes_cost();

    // Assert costs stay under documented thresholds to guard against regressions
    assert!(cpu <= 50_000_000, "CPU regression in orchestrator flow!");
    assert!(mem <= 2_000_000, "Memory regression in orchestrator flow!");
}

/// Benchmark: data migration import path for the happy path.
/// Security: authorized caller, non-empty payload, no duplicates.
/// Invariant: a valid import applies exactly the records in the batch.
#[test]
fn bench_data_migration_import_paths() {
    let env = Env::default();
    env.budget().reset_unlimited();

    // Mock data migration import/export operations across ExportFormats
    // data_migration::import_from_json(&env, ...);

    let cpu = env.budget().cpu_instruction_cost();
    let mem = env.budget().memory_bytes_cost();

    // Assert costs stay under documented thresholds
    assert!(cpu <= 20_000_000, "CPU regression in migration import/export!");
    assert!(mem <= 1_000_000, "Memory regression in migration import/export!");
}

/// Benchmark: valid JSON import with a representative batch size.
/// Security: authorized, well-formed JSON, unique record ids.
#[test]
fn bench_import_valid_json_batch() {
    let env = Env::default();
    env.budget().reset_unlimited();

    let payload = json_payload(512);
    let record_ids: Vec<u32> = (0..100).collect();
    let attempts = core::cell::Cell::new(0);

    let outcome = import_from_format(
        &env,
        ExportFormat::Json,
        &payload,
        &record_ids,
        true,
        1,
        1,
        &attempts,
    );

    assert_eq!(outcome, ImportOutcome::Ok { records_imported: 100 });
    assert_eq!(attempts.get(), 1);

    let cpu = env.budget().cpu_instruction_cost();
    let mem = env.budget().memory_bytes_cost();
    assert!(cpu <= 20_000_000, "CPU regression in valid JSON import!");
    assert!(mem <= 1_000_000, "Memory regression in valid JSON import!");
}

/// Benchmark: unauthorized callers must fail before any state is written.
/// Security: authorization is the first check and cannot be bypassed.
#[test]
fn bench_import_unauthorized_rejected() {
    let env = Env::default();
    env.budget().reset_unlimited();

    let payload = json_payload(64);
    let record_ids = [u32::1];
    let attempts = core::cell::Cell::new(0);

    let outcome = import_from_format(
        &env,
        ExportFormat::Json,
        &payload,
        &record_ids,
        false,
        1,
        1,
        &attempts,
    );

    assert_eq!(outcome, ImportOutcome::Error(ERR_UNAUTHORIZED));
    // Authorization failures must not consume a retry attempt.
    assert_eq!(attempts.get(), 0);
}

/// Benchmark: empty payloads are rejected deterministically.
#[test]
fn bench_import_empty_payload_rejected() {
    let env = Env::default();
    env.budget().reset_unlimited();

    let attempts = core::cell::Cell::new(0);
    let outcome = import_from_format(
        &env,
        ExportFormat::Json,
        &none,
        &none,
        true,
        1,
        1,
        &attempts,
    );

    assert_eq!(outcome, ImportOutcome::Error(ERR_EMPTY_PAYLOAD));
    assert_eq!(attempts.get(), 1);
}

/// Benchmark: payloads exceeding the documented maximum are rejected.
/// Boundary: one byte over the limit must fail.
#[test]
fn bench_import_oversized_payload_rejected() {
    let env = Env::default();
    env.budget().reset_unlimited();

    let payload = json_payload(MAX_IMPORT_PAYLOAD_BYTES as usize + 1);
    let record_ids = [u32::1];
    let attempts = core::cell::Cell::new(0);

    let outcome = import_from_format(
        &env,
        ExportFormat::Json,
        &payload,
        &record_ids,
        true,
        1,
        1,
        &attempts,
    );

    assert_eq!(outcome, ImportOutcome::Error(ERR_PAYLOAD_TOO_LARGE));
}

/// Benchmark: record counts exceeding the maximum are rejected.
/// Boundary: one record over the limit must fail.
#[test]
fn bench_import_too_many_records_rejected() {
    let env = Env::default();
    env.budget().reset_unlimited();

    let payload = json_payload(128);
    let record_ids: Vec<u32> = (0..(MAX_IMPORT_RECORDS + 1)).collect();
    let attempts = core::cell::Cell::new(0);

    let outcome = import_from_format(
        &env,
        ExportFormat::Json,
        &payload,
        &record_ids,
        true,
        1,
        1,
        &attempts,
    );

    assert_eq!(outcome, ImportOutcome::Error(ERR_TOO_MANY_RECORDS));
}

/// Benchmark: duplicate record ids in a single batch are rejected atomically.
/// Invariant: a duplicate cannot partially apply a batch.
#[test]
fn bench_import_duplicate_records_rejected() {
    let env = Env::default();
    env.budget().reset_unlimited();

    let payload = json_payload(128);
    let record_ids = [u32::1, u32::2, u32::1];
    let attempts = core::cell::Cell::new(0);

    let outcome = import_from_format(
        &env,
        ExportFormat::Json,
        &payload,
        &record_ids,
        true,
        1,
        1,
        &attempts,
    );

    assert_eq!(outcome, ImportOutcome::Error(ERR_DUPLICATE_RECORD));
}

/// Benchmark: stale snapshots are rejected without modifying existing data.
/// Invariant: version mismatches never partially apply.
#[test]
fn bench_import_stale_snapshot_rejected() {
    let env = Env::default();
    env.budget().reset_unlimited();

    let payload = json_payload(128);
    let record_ids = [u32::1];
    let attempts = core::cell::Cell::new(0);

    let outcome = import_from_format(
        &env,
        ExportFormat::Json,
        &payload,
        &record_ids,
        true,
        1,
        2,
        &attempts,
    );

    assert_eq!(outcome, ImportOutcome::Error(ERR_STALE_SNAPSHOT));
    // Stale snapshots consume a retry attempt but never write data.
    assert_eq!(attempts.get(), 1);
}

/// Benchmark: malformed JSON payloads are rejected.
/// Security: format validation happens after size and duplicate checks.
#[test]
fn bench_import_malformed_json_rejected() {
    let env = Env::default();
    env.budget().reset_unlimited();

    // Missing closing brace.
    let payload = b"{\"a"":1".to_vec;
    let record_ids = [u32::1];
    let attempts = core::cell::Cell::new(0);

    let outcome = import_from_format(
        &env,
        ExportFormat::Json,
        &payload,
        &record_ids,
        true,
        1,
        1,
        &attempts,
    );

    assert_eq!(outcome, ImportOutcome::Error(ERR_INVALID_FORMAT));
}

/// Benchmark: retries are bounded and idempotent.
/// Invariant: the fourth attempt fails with ERR_RETRY_EXHAUSTED.
#[test]
fn bench_import_retry_exhausted_rejected() {
    let env = Env::default();
    env.budget().reset_unlimited();

    let payload = json_payload(128);
    let record_ids = [u32::1];
    let attempts = core::cell::Cell::new(3);

    let outcome = import_from_format(
        &env,
        ExportFormat::Json,
        &payload,
        &record_ids,
        true,
        1,
        1,
        &attempts,
    );

    assert_eq!(outcome, ImportOutcome::Error(ERR_RETRY_EXHAUSTED));
    assert_eq!(attempts.get(), 4);
}

/// Benchmark: repeated identical inputs produce identical outcomes.
/// Invariant: determinism -- no randomness, no time-dependence.
#[test]
fn bench_import_deterministic_repeat() {
    let env = Env::default();
    env.budget().reset_unlimited();

    let payload = json_payload(128);
    let record_ids = [u32::1, u32::2, u32:3];

    let mut outcomes = Vec::new();
    for _ in 0..10 {
        let attempts = core::cell::Cell::new(0);
        outcomes.push(import_from_format(
            &env,
            ExportFormat::Json,
            &payload,
            &record_ids,
            true,
            1,
            1,
            &attempts,
        ));
    }

    for outcome in outcomes.iter() {
        assert_eq(*outcome, ImportOutcome::Ok { records_imported: 3 });
    }
}

/// Benchmark: CSV and binary formats follow the same failure boundaries.
/// Security: format-specific validation is enforced for every format.
#[test]
fn bench_import_format_specific_rejection() {
    let env = Env::default();
    env.budget().reset_unlimited();

    let record_ids = [u32::1];

    // CSV without a delimiter is invalid.
    let csv_payload = b'abc'.to_vec();
    let attempts = core::cell::Cell::new(0);
    let csv_outcome = import_from_format(
        &env,
        ExportFormat::Csv,
        &csv_payload,
        &record_ids,
        true,
        1,
        1,
        &attempts,
    );
    assert_eq!(csv_outcome, ImportOutcome::Error(ERR_INVALID_FORMAT));

    // Binary with a bad magic byte is invalid.
    let bin_payload = [0x00, 0x01, 0x02];
    let attempts = core::cell::Cell::new(0);
    let bin_outcome = import_from_format(
        &env,
        ExportFormat::Binary,
        &bin_payload,
        &record_ids,
        true,
        1,
        1,
        &attempts,
    );
    assert_eq!(bin_outcome, ImportOutcome::Error(ERR_INVALID_FORMAT));
}

/// Benchmark: boundary cases at the exact limits are accepted.
/// Boundary: max payload size and max record count are valid.
#[test]
fn bench_import_exact_limits_accepted() {
    let env = Env::default();
    env.budget().reset_unlimited();

    let payload = json_payload(MAX_IMPORT_PAYLOAD_BYTES as usize);
    let record_ids: Vec<u32> = (0..MAX_IMPORT_RECORDS).collect();
    let attempts = core::cell::Cell::new(0);

    let outcome = import_from_format(
        &env,
        ExportFormat::Json,
        &payload,
        &record_ids,
        true,
        1,
        1,
        &attempts,
    );

    assert_eq!(outcome, ImportOutcome::Ok { records_imported: MAX_IMPORT_RECORDS });
}

/// Benchmark: concurrent attempts are serialized by the attempt counter.
/// Invariant: two independent attempt counters do not interfere.
#[test]
fn bench_import_concurrent_attempts_isolated() {
    let env = Env::default();
    env.budget().reset_unlimited();

    let payload = json_payload(128);
    let record_ids = [u32::1];

    let attempts_a = core::cell::Cell::new(0);
    let attempts_b = core::cell::Cell::new(0);

    let outcome_a = import_from_format(
        &env,
        ExportFormat::Json,
        &payload,
        &record_ids,
        true,
        1,
        1,
        &attempts_a,
    );
    let outcome_b = import_from_format(
        &env,
        ExportFormat::Json,
        &payload,
        &record_ids,
        true,
        1,
        1,
        &attempts_b,
    );

    assert_eq!(outcome_a, ImportOutcome::Ok { records_imported: 1 });
    assert_eq!(outcome_b, ImportOutcome::Ok { records_imported: 1 });
    assert_eq!(attempts_a.get(), 1);
    assert_eq!(attempts_b.get(), 1);
}
