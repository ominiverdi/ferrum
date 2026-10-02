use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::VecDeque;

const HARD_TOOL_ROUND_LIMIT: usize = 256;
const REPEATED_TOOL_NUDGE_LIMIT: usize = 4;
const REPEATED_TOOL_FORCE_LIMIT: usize = 7;
const CONSECUTIVE_ERROR_NUDGE_LIMIT: usize = 5;
const CONSECUTIVE_ERROR_FORCE_LIMIT: usize = 8;
const MAX_SEQUENCE_LENGTH: usize = 16;
const SEQUENCE_NUDGE_REPEATS: usize = 2;
const SEQUENCE_FORCE_REPEATS: usize = 3;
const SEQUENCE_HISTORY_LIMIT: usize = MAX_SEQUENCE_LENGTH * SEQUENCE_FORCE_REPEATS;

type Fingerprint = [u8; 32];

#[derive(Debug)]
pub(super) struct ToolObservation {
    fingerprint: Fingerprint,
    sequence_fingerprint: Option<Fingerprint>,
    is_error: bool,
}

impl ToolObservation {
    pub(super) fn new(name: &str, input: &Value, content: &str, is_error: bool) -> Self {
        let mut hash = Sha256::new();
        hash.update(name.as_bytes());
        hash.update([0]);
        hash.update(input.to_string().as_bytes());
        let fingerprint = hash.finalize().into();

        // Deliberate foreground polling and successful native mutations break
        // sequence adjacency. Changed results also distinguish useful work.
        let sequence_fingerprint =
            if name == "wait" || (!is_error && matches!(name, "write" | "edit")) {
                None
            } else {
                let mut hash = Sha256::new();
                hash.update(fingerprint);
                hash.update([u8::from(is_error)]);
                hash.update(content.as_bytes());
                Some(hash.finalize().into())
            };
        Self {
            fingerprint,
            sequence_fingerprint,
            is_error,
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum LoopGuardAction {
    Continue,
    Nudge(String),
    ForceFinal(String),
}

#[derive(Debug)]
pub(super) struct LoopGuard {
    explicit_limit: usize,
    rounds: usize,
    consecutive_errors: usize,
    last_tool_fingerprint: Option<Fingerprint>,
    consecutive_tool_repeats: usize,
    repeated_nudged: bool,
    errors_nudged: bool,
    sequence_history: VecDeque<Fingerprint>,
    sequence_nudged: bool,
}

impl LoopGuard {
    pub(super) fn new(explicit_limit: usize) -> Self {
        Self {
            explicit_limit,
            rounds: 0,
            consecutive_errors: 0,
            last_tool_fingerprint: None,
            consecutive_tool_repeats: 0,
            repeated_nudged: false,
            errors_nudged: false,
            sequence_history: VecDeque::new(),
            sequence_nudged: false,
        }
    }

    pub(super) fn observe_round(&mut self, observations: &[ToolObservation]) -> LoopGuardAction {
        self.rounds += 1;
        if self.explicit_limit > 0 && self.rounds >= self.explicit_limit {
            return LoopGuardAction::ForceFinal(format!(
                "explicit tool round limit ({}) reached",
                self.explicit_limit
            ));
        }
        if self.rounds >= HARD_TOOL_ROUND_LIMIT {
            return LoopGuardAction::ForceFinal(format!(
                "hard safety limit ({HARD_TOOL_ROUND_LIMIT}) reached"
            ));
        }

        let mut max_repeats = 0;
        for observation in observations {
            if self.last_tool_fingerprint == Some(observation.fingerprint) {
                self.consecutive_tool_repeats += 1;
            } else {
                self.last_tool_fingerprint = Some(observation.fingerprint);
                self.consecutive_tool_repeats = 1;
                self.repeated_nudged = false;
            }
            max_repeats = max_repeats.max(self.consecutive_tool_repeats);

            if observation.is_error {
                self.consecutive_errors += 1;
            } else {
                self.consecutive_errors = 0;
            }

            if let Some(fingerprint) = observation.sequence_fingerprint {
                if self.sequence_history.len() == SEQUENCE_HISTORY_LIMIT {
                    self.sequence_history.pop_front();
                }
                self.sequence_history.push_back(fingerprint);
            } else {
                self.sequence_history.clear();
                self.sequence_nudged = false;
            }
        }

        if max_repeats >= REPEATED_TOOL_FORCE_LIMIT {
            return LoopGuardAction::ForceFinal(format!(
                "same tool call repeated {max_repeats} times"
            ));
        }
        if self.consecutive_errors >= CONSECUTIVE_ERROR_FORCE_LIMIT {
            return LoopGuardAction::ForceFinal(format!(
                "{} consecutive tool errors",
                self.consecutive_errors
            ));
        }
        if let Some(length) = self.repeated_sequence(SEQUENCE_FORCE_REPEATS) {
            return LoopGuardAction::ForceFinal(format!(
                "{length}-call tool sequence repeated {SEQUENCE_FORCE_REPEATS} times with unchanged results"
            ));
        }
        if max_repeats >= REPEATED_TOOL_NUDGE_LIMIT && !self.repeated_nudged {
            self.repeated_nudged = true;
            return LoopGuardAction::Nudge(format!("same tool call repeated {max_repeats} times"));
        }

        if self.consecutive_errors >= CONSECUTIVE_ERROR_NUDGE_LIMIT && !self.errors_nudged {
            self.errors_nudged = true;
            return LoopGuardAction::Nudge(format!(
                "{} consecutive tool errors",
                self.consecutive_errors
            ));
        }

        if let Some(length) = self.repeated_sequence(SEQUENCE_NUDGE_REPEATS) {
            if !self.sequence_nudged {
                self.sequence_nudged = true;
                return LoopGuardAction::Nudge(format!(
                    "{length}-call tool sequence repeated {SEQUENCE_NUDGE_REPEATS} times with unchanged results"
                ));
            }
        } else {
            self.sequence_nudged = false;
        }
        LoopGuardAction::Continue
    }

    fn repeated_sequence(&self, repeats: usize) -> Option<usize> {
        let n = self.sequence_history.len();
        (2..=MAX_SEQUENCE_LENGTH.min(n / repeats)).find(|&length| {
            let start = n - length * repeats;
            // Single-call repetition retains its existing, less aggressive policy.
            let distinct = (1..length)
                .any(|i| self.sequence_history[start + i] != self.sequence_history[start]);
            distinct
                && (length..length * repeats).all(|i| {
                    self.sequence_history[start + i] == self.sequence_history[start + i % length]
                })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn observation(name: &str, input: Value, is_error: bool) -> ToolObservation {
        ToolObservation::new(name, &input, "unchanged result", is_error)
    }

    #[test]
    fn nudges_then_forces_repeated_tool_calls() {
        let mut guard = LoopGuard::new(0);
        let read = observation("read", json!({"path": "a.txt"}), false);
        for _ in 0..3 {
            assert_eq!(
                guard.observe_round(std::slice::from_ref(&read)),
                LoopGuardAction::Continue
            );
        }
        assert!(matches!(guard.observe_round(std::slice::from_ref(&read)),
            LoopGuardAction::Nudge(reason) if reason.contains("same tool call repeated")));
        for _ in 0..2 {
            assert_eq!(
                guard.observe_round(std::slice::from_ref(&read)),
                LoopGuardAction::Continue
            );
        }
        assert!(matches!(guard.observe_round(std::slice::from_ref(&read)),
            LoopGuardAction::ForceFinal(reason) if reason.contains("same tool call repeated")));
    }

    #[test]
    fn separated_identical_calls_do_not_accumulate_repetition_count() {
        let mut guard = LoopGuard::new(0);
        let read_a = observation("read", json!({"path": "a.txt"}), false);
        let read_b = observation("read", json!({"path": "b.txt"}), false);
        for _ in 0..3 {
            assert_eq!(
                guard.observe_round(std::slice::from_ref(&read_a)),
                LoopGuardAction::Continue
            );
        }
        assert_eq!(
            guard.observe_round(std::slice::from_ref(&read_b)),
            LoopGuardAction::Continue
        );
        for _ in 0..3 {
            assert_eq!(
                guard.observe_round(std::slice::from_ref(&read_a)),
                LoopGuardAction::Continue
            );
        }
        assert!(matches!(guard.observe_round(std::slice::from_ref(&read_a)),
            LoopGuardAction::Nudge(reason) if reason.contains("same tool call repeated")));
    }

    #[test]
    fn nudges_consecutive_tool_errors() {
        let mut guard = LoopGuard::new(0);
        for index in 0..4 {
            let failed = observation("edit", json!({"path": format!("{index}.txt")}), true);
            assert_eq!(
                guard.observe_round(std::slice::from_ref(&failed)),
                LoopGuardAction::Continue
            );
        }
        let failed = observation("edit", json!({"path": "final.txt"}), true);
        assert!(matches!(guard.observe_round(std::slice::from_ref(&failed)),
            LoopGuardAction::Nudge(reason) if reason.contains("consecutive tool errors")));
    }

    #[test]
    fn explicit_limit_forces_final() {
        let mut guard = LoopGuard::new(2);
        let read = observation("read", json!({"path": "a.txt"}), false);
        assert_eq!(
            guard.observe_round(std::slice::from_ref(&read)),
            LoopGuardAction::Continue
        );
        assert!(matches!(guard.observe_round(std::slice::from_ref(&read)),
            LoopGuardAction::ForceFinal(reason) if reason.contains("explicit tool round limit")));
    }

    #[test]
    fn alternating_calls_nudge_then_force_final() {
        let mut guard = LoopGuard::new(0);
        let a = observation("read", json!({"path": "a"}), false);
        let b = observation("grep", json!({"pattern": "b"}), false);
        for call in [&a, &b, &a] {
            assert_eq!(
                guard.observe_round(std::slice::from_ref(call)),
                LoopGuardAction::Continue
            );
        }
        assert!(matches!(guard.observe_round(std::slice::from_ref(&b)),
            LoopGuardAction::Nudge(reason) if reason.contains("2-call tool sequence")));
        assert_eq!(
            guard.observe_round(std::slice::from_ref(&a)),
            LoopGuardAction::Continue
        );
        assert!(matches!(guard.observe_round(std::slice::from_ref(&b)),
            LoopGuardAction::ForceFinal(reason) if reason.contains("2-call tool sequence")));
    }

    #[test]
    fn detects_longer_sequences_across_batch_boundaries() {
        let mut guard = LoopGuard::new(0);
        let calls = [
            observation("read", json!({"path": "a"}), false),
            observation("read", json!({"path": "b"}), false),
            observation("read", json!({"path": "c"}), false),
        ];
        assert_eq!(guard.observe_round(&calls), LoopGuardAction::Continue);
        assert!(matches!(
            guard.observe_round(&calls),
            LoopGuardAction::Nudge(_)
        ));
        assert!(matches!(
            guard.observe_round(&calls),
            LoopGuardAction::ForceFinal(_)
        ));
    }

    #[test]
    fn changes_and_polling_break_sequence_adjacency() {
        for breaker in ["write", "edit", "wait"] {
            let mut guard = LoopGuard::new(0);
            for _ in 0..10 {
                let calls = [
                    observation("bash", json!({"command": "cargo test"}), false),
                    observation(breaker, json!({"path": "a"}), false),
                ];
                assert_eq!(guard.observe_round(&calls), LoopGuardAction::Continue);
            }
        }
    }

    #[test]
    fn changing_results_are_not_a_repeating_sequence() {
        let mut guard = LoopGuard::new(0);
        for i in 0..10 {
            let calls = [
                ToolObservation::new("read", &json!({"path": "a"}), &i.to_string(), false),
                observation("read", json!({"path": "b"}), false),
            ];
            assert_eq!(guard.observe_round(&calls), LoopGuardAction::Continue);
        }
    }

    #[test]
    fn different_action_allows_a_later_legitimate_retry() {
        let mut guard = LoopGuard::new(0);
        for cycle in 0..10 {
            let calls = [
                observation("read", json!({"path": "a"}), false),
                observation("read", json!({"path": "b"}), false),
                observation("grep", json!({"pattern": cycle}), false),
            ];
            assert_eq!(guard.observe_round(&calls), LoopGuardAction::Continue);
        }
    }

    #[test]
    fn detects_maximum_length_sequence_and_resets_for_a_new_turn() {
        let calls = (0..MAX_SEQUENCE_LENGTH)
            .map(|i| observation("read", json!({"path":i}), false))
            .collect::<Vec<_>>();
        let mut guard = LoopGuard::new(0);
        assert_eq!(guard.observe_round(&calls), LoopGuardAction::Continue);
        assert!(matches!(
            guard.observe_round(&calls),
            LoopGuardAction::Nudge(_)
        ));
        assert!(matches!(
            guard.observe_round(&calls),
            LoopGuardAction::ForceFinal(_)
        ));
        let mut next_turn = LoopGuard::new(0);
        assert_eq!(next_turn.observe_round(&calls), LoopGuardAction::Continue);
    }

    #[test]
    fn failed_edits_do_not_reset_sequence_detection() {
        let calls = [
            observation("edit", json!({"path":"a"}), true),
            observation("edit", json!({"path":"b"}), true),
        ];
        let mut guard = LoopGuard::new(0);
        assert_eq!(guard.observe_round(&calls), LoopGuardAction::Continue);
        assert!(
            matches!(guard.observe_round(&calls), LoopGuardAction::Nudge(reason) if reason.contains("2-call tool sequence"))
        );
        assert!(
            matches!(guard.observe_round(&calls), LoopGuardAction::ForceFinal(reason) if reason.contains("2-call tool sequence"))
        );
    }

    #[test]
    fn hard_round_limit_and_error_escalation_are_preserved() {
        let mut guard = LoopGuard::new(0);
        guard.rounds = HARD_TOOL_ROUND_LIMIT - 1;
        assert!(
            matches!(guard.observe_round(&[]), LoopGuardAction::ForceFinal(reason) if reason.contains("hard safety limit"))
        );
        let mut guard = LoopGuard::new(0);
        for i in 0..7 {
            let call = observation("edit", json!({"path":i}), true);
            guard.observe_round(&[call]);
        }
        assert!(
            matches!(guard.observe_round(&[observation("edit", json!({"path":8}), true)]),
            LoopGuardAction::ForceFinal(reason) if reason.contains("8 consecutive tool errors"))
        );
    }

    #[test]
    fn sequence_history_and_diagnostics_are_bounded() {
        let mut guard = LoopGuard::new(0);
        let calls = (0..200)
            .map(|i| observation("read", json!({"path": i, "secret": "sentinel"}), false))
            .collect::<Vec<_>>();
        assert_eq!(guard.observe_round(&calls), LoopGuardAction::Continue);
        assert_eq!(guard.sequence_history.len(), SEQUENCE_HISTORY_LIMIT);
        let a = observation("read", json!({"path": "sentinel"}), false);
        let b = observation("grep", json!({"pattern": "sentinel"}), false);
        guard.observe_round(&[a, b]);
        let action = guard.observe_round(&[
            observation("read", json!({"path": "sentinel"}), false),
            observation("grep", json!({"pattern": "sentinel"}), false),
        ]);
        assert!(matches!(action, LoopGuardAction::Nudge(reason) if !reason.contains("sentinel")));
    }
}
