//! The broker security-freeze state machine (§42, §43, §88).
//!
//! The daemon's lifecycle is explicit and every transition is checked:
//!
//! ```text
//! Initializing ──▶ DeviceConfigured ──▶ Sandboxed ──▶ Serving ──▶ ShuttingDown
//!      │                                                        ▲
//!      └─────────────────────────────────────────────────────────┘ (failure)
//! ```
//!
//! Illegal transitions fail: `Serving → configure device`, `Serving →
//! regain capability`, `Serving → open new device` are all impossible
//! because the state machine forbids them and the seccomp freeze makes them
//! fail at the kernel anyway (§43).
//!
//! The sandbox is **irreversible**: once [`BrokerPhase::Serving`] is entered
//! there is no supported operation that re-enables root, capabilities,
//! uinput configuration, arbitrary file opens, networking, or broader
//! syscalls. Reconfiguration requires a daemon restart (§43, §44).

use std::fmt;

/// The lifecycle phase of the broker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BrokerPhase {
    /// Startup: opening/verifying the device, binding the listener — before
    /// any hostile input is accepted (§41).
    Initializing,
    /// The kernel device is created and verified; the security freeze is
    /// next (§8).
    DeviceConfigured,
    /// Privileges dropped, NoNewPrivs set, seccomp installed, state
    /// verified (§41, §42).
    Sandboxed,
    /// Accepting untrusted clients (§8: never before this point).
    Serving,
    /// Releasing held keys and exiting (§81).
    ShuttingDown,
}

impl fmt::Display for BrokerPhase {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            BrokerPhase::Initializing => "initializing",
            BrokerPhase::DeviceConfigured => "device-configured",
            BrokerPhase::Sandboxed => "sandboxed",
            BrokerPhase::Serving => "serving",
            BrokerPhase::ShuttingDown => "shutting-down",
        };
        f.write_str(s)
    }
}

/// Errors from phase transitions.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PhaseError {
    #[error("illegal phase transition: {from} -> {to}")]
    IllegalTransition { from: BrokerPhase, to: BrokerPhase },
    #[error("operation requires phase {required}, but the broker is in {actual}")]
    WrongPhase {
        required: BrokerPhase,
        actual: BrokerPhase,
    },
}

/// An explicitly tracked phase with enforced transitions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PhaseGuard {
    phase: BrokerPhase,
}

impl PhaseGuard {
    pub fn new() -> Self {
        PhaseGuard {
            phase: BrokerPhase::Initializing,
        }
    }

    pub fn phase(&self) -> BrokerPhase {
        self.phase
    }

    /// Assert the broker is in exactly `expected`.
    pub fn expect(&self, expected: BrokerPhase) -> Result<(), PhaseError> {
        if self.phase == expected {
            Ok(())
        } else {
            Err(PhaseError::WrongPhase {
                required: expected,
                actual: self.phase,
            })
        }
    }

    /// The allowed forward transitions (a small, exhaustively-tested set).
    fn allows(from: BrokerPhase, to: BrokerPhase) -> bool {
        matches!(
            (from, to),
            (
                BrokerPhase::Initializing,
                BrokerPhase::DeviceConfigured | BrokerPhase::ShuttingDown
            ) | (
                BrokerPhase::DeviceConfigured,
                BrokerPhase::Sandboxed | BrokerPhase::ShuttingDown
            ) | (
                BrokerPhase::Sandboxed,
                BrokerPhase::Serving | BrokerPhase::ShuttingDown
            ) | (BrokerPhase::Serving, BrokerPhase::ShuttingDown)
        )
    }

    /// Transition from `from` to `to`; fails on any other pair (§42).
    pub fn transition(&mut self, from: BrokerPhase, to: BrokerPhase) -> Result<(), PhaseError> {
        if self.phase != from {
            return Err(PhaseError::WrongPhase {
                required: from,
                actual: self.phase,
            });
        }
        if Self::allows(from, to) {
            self.phase = to;
            Ok(())
        } else {
            Err(PhaseError::IllegalTransition { from, to })
        }
    }
}

impl Default for PhaseGuard {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn starts_in_initializing() {
        assert_eq!(PhaseGuard::new().phase(), BrokerPhase::Initializing);
    }

    #[test]
    fn happy_path_reaches_serving() {
        let mut g = PhaseGuard::new();
        g.transition(BrokerPhase::Initializing, BrokerPhase::DeviceConfigured)
            .unwrap();
        g.transition(BrokerPhase::DeviceConfigured, BrokerPhase::Sandboxed)
            .unwrap();
        g.transition(BrokerPhase::Sandboxed, BrokerPhase::Serving)
            .unwrap();
        g.transition(BrokerPhase::Serving, BrokerPhase::ShuttingDown)
            .unwrap();
    }

    #[test]
    fn skips_are_rejected() {
        let mut g = PhaseGuard::new();
        // Initializing -> Serving (skipping the freeze) is illegal.
        assert_eq!(
            g.transition(BrokerPhase::Initializing, BrokerPhase::Serving),
            Err(PhaseError::IllegalTransition {
                from: BrokerPhase::Initializing,
                to: BrokerPhase::Serving,
            })
        );
    }

    #[test]
    fn every_illegal_transition_is_rejected_exhaustively() {
        // §42: "Illegal transitions must fail" — prove every unordered pair
        // that is not in the allowed set fails, and every allowed one passes.
        let all = [
            BrokerPhase::Initializing,
            BrokerPhase::DeviceConfigured,
            BrokerPhase::Sandboxed,
            BrokerPhase::Serving,
            BrokerPhase::ShuttingDown,
        ];
        for &from in &all {
            for &to in &all {
                let mut g = PhaseGuard::new();
                // Walk the happy path until the guard reaches `from`.
                while g.phase() != from {
                    let next = match g.phase() {
                        BrokerPhase::Initializing => BrokerPhase::DeviceConfigured,
                        BrokerPhase::DeviceConfigured => BrokerPhase::Sandboxed,
                        BrokerPhase::Sandboxed => BrokerPhase::Serving,
                        BrokerPhase::Serving => BrokerPhase::ShuttingDown,
                        BrokerPhase::ShuttingDown => break,
                    };
                    g.transition(g.phase(), next).unwrap();
                }
                assert_eq!(g.phase(), from);
                let result = g.transition(from, to);
                if PhaseGuard::allows(from, to) {
                    assert_eq!(result, Ok(()), "{from:?} -> {to:?} must be allowed");
                    assert_eq!(g.phase(), to);
                } else {
                    assert!(
                        matches!(result, Err(PhaseError::IllegalTransition { .. })),
                        "{from:?} -> {to:?} must be rejected"
                    );
                    assert_eq!(g.phase(), from, "state must not change on failure");
                }
            }
        }
    }

    #[test]
    fn serving_cannot_regress_to_configure_devices() {
        let mut g = PhaseGuard::new();
        g.transition(BrokerPhase::Initializing, BrokerPhase::DeviceConfigured)
            .unwrap();
        g.transition(BrokerPhase::DeviceConfigured, BrokerPhase::Sandboxed)
            .unwrap();
        g.transition(BrokerPhase::Sandboxed, BrokerPhase::Serving)
            .unwrap();
        // §43: no supported operation re-enables device configuration.
        assert!(g
            .transition(BrokerPhase::Serving, BrokerPhase::DeviceConfigured)
            .is_err());
        // §43: no supported operation re-enables the earlier phases.
        assert!(g
            .transition(BrokerPhase::Serving, BrokerPhase::Sandboxed)
            .is_err());
    }
}
