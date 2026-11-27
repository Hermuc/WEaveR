//! Pure decision rules for the download manager.
//!
//! These carry the retry and exit policy that used to be inlined inside the
//! 290-line `start` orchestrator. Nothing here does I/O, sleeps, or touches
//! Tauri state, so every branch is testable without spawning
//! DepotDownloader.

use std::time::Duration;

/// Silence budget for one attempt, measured from the last line of output.
///
/// Before the transfer begins a dead Steam CM shows up as a short silence;
/// once transferring, a healthy-but-quiet download can legitimately say
/// nothing for minutes, so that budget is far longer.
pub(super) const CONNECT_BUDGET: Duration = Duration::from_secs(45);
pub(super) const STALL_BUDGET_WITH_PROGRESS: Duration = Duration::from_secs(300);

/// Pause before retrying with the next account, so a rate-limited login does
/// not hammer Steam.
pub(super) const RETRY_BACKOFF: Duration = Duration::from_secs(2);

/// Which silence budget applies to the current attempt.
pub(super) fn idle_budget(download_started: bool) -> Duration {
    if download_started {
        STALL_BUDGET_WITH_PROGRESS
    } else {
        CONNECT_BUDGET
    }
}

/// Which account index the next attempt uses.
///
/// A custom account is pinned to the one the user picked — retrying with a
/// different account would violate that choice. Built-in accounts rotate so a
/// rate-limited login gets a fresh identity.
pub(super) fn next_account_index(
    current: usize,
    starting: usize,
    total: usize,
    is_custom: bool,
) -> usize {
    if is_custom {
        starting
    } else if total == 0 {
        0
    } else {
        (current + 1) % total
    }
}

/// Whether the account rotation has run out of attempts.
pub(super) fn retries_exhausted(
    attempt: usize,
    total_accounts: usize,
    infinite_retry: bool,
) -> bool {
    !infinite_retry && attempt >= total_accounts
}

/// Terminal classification for one attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum AttemptOutcome {
    Completed,
    Retry,
}

/// Classify one finished attempt.
///
/// A run counts as complete when DepotDownloader exited cleanly *or* it left
/// usable files behind, and the connection never dropped. The `has_output`
/// fallback matters because the tool sometimes exits non-zero after finishing.
pub(super) fn attempt_outcome(
    exit_ok: bool,
    has_output: bool,
    connection_failed: bool,
) -> AttemptOutcome {
    if (exit_ok || has_output) && !connection_failed {
        AttemptOutcome::Completed
    } else {
        AttemptOutcome::Retry
    }
}

/// Status text shown when every account has failed.
pub(super) fn failure_text(stderr_tail: &[String]) -> String {
    if stderr_tail.is_empty() {
        "Failed".to_string()
    } else {
        format!("Failed: {}", stderr_tail.join(" | "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn idle_budget_is_shorter_before_the_transfer_starts() {
        assert_eq!(idle_budget(false), CONNECT_BUDGET);
        assert_eq!(idle_budget(true), STALL_BUDGET_WITH_PROGRESS);
        assert!(idle_budget(true) > idle_budget(false));
    }

    #[test]
    fn a_custom_account_never_rotates() {
        for current in [0, 1, 5] {
            assert_eq!(next_account_index(current, 3, 9, true), 3);
        }
    }

    #[test]
    fn builtin_accounts_rotate_and_wrap() {
        assert_eq!(next_account_index(0, 0, 3, false), 1);
        assert_eq!(next_account_index(2, 0, 3, false), 0);
        assert_eq!(next_account_index(0, 0, 1, false), 0);
    }

    #[test]
    fn a_zero_account_pool_does_not_divide_by_zero() {
        assert_eq!(next_account_index(0, 0, 0, false), 0);
    }

    #[test]
    fn retries_are_exhausted_after_one_pass_over_the_accounts() {
        assert!(!retries_exhausted(0, 3, false));
        assert!(!retries_exhausted(2, 3, false));
        assert!(retries_exhausted(3, 3, false));
        assert!(retries_exhausted(4, 3, false));
    }

    #[test]
    fn infinite_retry_never_exhausts() {
        assert!(!retries_exhausted(1000, 3, true));
    }

    #[test]
    fn a_clean_exit_is_completed() {
        assert_eq!(
            attempt_outcome(true, false, false),
            AttemptOutcome::Completed
        );
    }

    #[test]
    fn leftover_files_count_as_completed_even_if_the_exit_was_dirty() {
        assert_eq!(
            attempt_outcome(false, true, false),
            AttemptOutcome::Completed
        );
    }

    #[test]
    fn a_dropped_connection_is_never_completed() {
        assert_eq!(attempt_outcome(true, true, true), AttemptOutcome::Retry);
        assert_eq!(attempt_outcome(false, false, true), AttemptOutcome::Retry);
    }

    #[test]
    fn no_stderr_gives_the_plain_failure_text() {
        assert_eq!(failure_text(&[]), "Failed");
    }

    #[test]
    fn stderr_tail_is_joined_into_the_failure_text() {
        let tail = vec![
            "ERROR: rate limited".to_string(),
            "ERROR: gave up".to_string(),
        ];
        assert_eq!(
            failure_text(&tail),
            "Failed: ERROR: rate limited | ERROR: gave up"
        );
    }
}
