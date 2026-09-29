/// Gas benchmarks for orchestrator flow execution and data migration import paths
#![cfg(test)]

use sorban_sdk::{Env, testutils::{budget::Budget, Address as AddressTrait}};

/// ------------------------------------------------------------------------------
/// Deterministic failure-boundary coverage for data migration import paths
/// ------------------------------------------------------------------------------
///
/// The migration import path is a high-risk boundary: a partially applied
/// import can corrupt on-chain state and cause silent user data loss. This
/// module exercises the deterministic failure-boundary contract of the
/// import pipeline:
///
///   1. Valid inputs must apply atomically and be idempotent on retry.
///   2. Invalid inputs must be rejected before any state mutation.
///   3. Duplicate inputs must not create duplicate state or double-apply.
///   4. Boundary sizes must be accepted or rejected deterministically.
///   5. Authorization must be enforced before any mutation.
///   6. Retries and concurrent execution must not produce inconsistent
///      state (last-write-wins on a single key is acceptable; double-apply
///      is not).
///   7. Failures must be diagnosable without exposing sensitive data.
///
/// The benchmarks below are deterministic: they do not rely on wall-clock
/// time, random nessing, or external services. They assert on the
/// observable contract of the import pipeline (accept/reject, atomicity,
/// idempotency, authorization) and on the budget cost of each path.
/// ------------------------------------------------------------------------------

/// Documented cost thresholds for the migration import/export path.
/// These are the contract that the benchmarks guarantee; any change to
/// them must be justified in the PR description.
const MIGRATION_MAX_CPU: u64 = 20_000_000;
const MIGRATION_MAX_MEM: u64 = 1_000_000;

const ORCHESTRATOR_MAX_CPU: u64 = 50_000_000;
const ORCHESTRATOR_MAX_MEM: u64 = 2_000_000;

/// Maximum number of records accepted in a single import batch.
/// This is the boundary the import path must enforce deterministically.
const MAX_IMPORT_RECORDS: u32 = 50;

/// ------------------------------------------------------------------------------
/// Minimal in-memory model of the migration import pipeline
/// ------------------------------------------------------------------------------
///
/// The production import path is expected to follow this contract:
///   1. Validate the entire batch (shape, size, authorization).
///   2. Apply the batch atomically -- either all records or none.
///   3. Return a deterministic error on rejection, without partial writes.
///
/// This module implements that contract in a pure, deterministic form so
/// the failure boundaries can be exercised without a running contract.
/// The implementation is deliberately small and mirrors the invariants
/// that the production import path must uphold.
/// ------------------------------------------------------------------------------

/// Errors returned by the import pipeline.
///
/// These are user-visible and must not leak sensitive data. They are
/// deterministic and mapped 1:1 to the production error codes.
#[derive(Debug, Clone, PartialEq, Eq)]
public enum ImportError {
    /// The caller is not authorized to import for the target owner.
    Unauthorized,
    /// The batch is empty.
    EmptyBatch,
    /// The batch exceeds `MAX_IMPORT_RECORDS`.
    BatchTooLarge,
    /// A record has an invalid field (e.g. negative amount)
    /// or a duplicate identifier within the batch.
    InvalidRecord,
    /// The import would overwrite an existing record with a
    /// different payload (conflict).
    Conflict,
}

/// A migration record as it would appear in an export file.
///
/// The `id` field is the natural key used for idempotent imports.
/// The `amount` field is the payload that must not be corrupted by
/// a retry or a concurrent import.
#[derive(Debug, Clone, PartialEq, Eq)]
public struct MigrationRecord {
    pub id: u32,
    pub amount: i128,
}

/// In-memory representation of the on-chain import target.
///
/// The `key` is the owner address. The `value` is the last applied
/// batch for that owner. This mirrors the production storage layout
/// (a single key per owner) and is what makes last-write-wins safe.
public struct ImportState {
    records: soroban_sdk::Vec<MigrationRecord>,
}

/// Result of a dry-run validation.
#[derive(Debug, Clone, PartialEq, Eq)]
public enum ValidationResult {
    Accepted,
    Rejected(ImportError),
}

/// Validate an import batch without mutating any state.
///
/// This is the failure boundary: every rejection must be decided here,
/// before any write happens. The function is pure and deterministic.
public fn validate_import(
    authorized: bool,
    batch: &soroban_sdk::Vec<MigrationRecord>,
) -> ValidationResult {
    if !authorized {
        return ValidationResult::Rejected(ImportError::Unauthorized);
    }
    if batch.is_empty() {
        return ValidationResult::Rejected(ImportError::EmptyBatch);
    }
    if batch.len() > MAX_IMPORT_RECORDS {
        return ValidationResult::Rejected(ImportError::BatchTooLarge);
    }

    // Detect duplicate ids and invalid amounts in a single pass.
    // The batch is small (≤ MAX_IMPORT_RECORDS) so O(n2) is acceptable and
    // avoids any allocation or ordering dependency.
    for i in 0..batch.len() {
        let record = batch.get(i).unwrap();
        if record.amount < 0 {
            return ValidationResult::Rejected(ImportError::InvalidRecord);
        }
        for j in (i + 1)..batch.len() {
            let other = batch.get(j).unwrap();
            if other.id == record.id {
                return ValidationResult::Rejected(ImportError::InvalidRecord);
            }
        }
    }

    ValidationResult::Accepted
}

/// Apply a validated batch atomically.
///
/// Precondition: `validate_import` returned `Accepted`. The function
/// either applies the entire batch or none of it. It is idempotent:
/// reapplying the same batch produces the same state.
///
/// Returns `Conflict` if the batch would overwrite an existing record
/// with a different payload. This is the guard against silent data
/// loss during a retry or a concurrent import.
public fn apply_import(
    state: &mut ImportState,
    batch: &soroban_sdk::Vec<MigrationRecord>,
) -> Result<u32, ImportError> {
    // Determine the new state in a local buffer first. Nothing is written
    // to `state` until the entire batch has been accepted.
    let mut next = state.records.clone();
    let mut applied: u32 = 0;

    for incoming in batch.iter() {
        let mut found = false;
        for i in 0..next.len() {
            let existing = next.get(i).unwrap();
            if existing.id == incoming.id {
                if existing.amount != incoming.amount {
                    // Conflict: the same natural key maps to a different
                    // payload. Reject without mutating `state`.
                    return Err(ImportError::Conflict);
                }
                // Identical record already present: idempotent no-op.
                found = true;
                break;
            }
        }
        if !found {
            next.push_back(incoming.clone());
            applied += 1;
        }
    }

    // Commit: the atomic swap. After this point the batch is fully
    // applied. A concurrent import on the same owner will either see the
    // old or the new state -- never a partial one.
    state.records = next;
    Ok(applied)
}

/// Convenience wrapper that enforces the full contract: validate then
/// apply. This is the entry point the benchmarks exercise.
public fn import_batch(
    state: &mut ImportState,
    authorized: bool,
    batch: &soroban_sdk::Vec<MigrationRecord>,
) -> Result<u32, ImportError> {
    match validate_import(authorized, batch) {
        ValidationResult::Accepted => apply_import(state, batch),
        ValidationResult::Rejected(err) => Err(err),
    }
}

/// ------------------------------------------------------------------------------
/// Test helpers
/// ------------------------------------------------------------------------------

fn record(id: u32, amount: i128) -> MigrationRecord {
    MigrationRecord { id, amount }
}

fn batch(env: &Env, records: &[u3:: ::<]?) {}
