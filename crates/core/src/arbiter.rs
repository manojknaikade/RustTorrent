//! Decides whether a torrent should be running.
//!
//! What the user asked for (`Intent`) is stored separately from the reasons the app may
//! hold a torrent back (`Holds`). Every feature that can stop a torrent (queue, schedule,
//! ratio limit, network binding) sets its own flag, and this one function combines them.
//! Releasing one hold therefore never overrides another hold or the user's own pause.
use serde::{Deserialize, Serialize};

/// What the user last asked for.
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Intent {
    #[default]
    Run,
    Pause,
}

/// Reasons the app itself is holding a torrent back.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Holds {
    /// The chosen network interface is not available.
    pub network: bool,
    /// The schedule says no transfers right now.
    pub schedule: bool,
    /// The seeding ratio limit was reached.
    pub ratio: bool,
    /// Waiting for a free slot in the queue.
    pub queue: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum HoldReason {
    Network,
    Schedule,
    Ratio,
    Queue,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Decision {
    Run,
    /// The user paused it.
    Paused,
    /// The user wants it running, but the app is holding it. The reason shown is the one
    /// the user most needs to know about.
    Held(HoldReason),
}

pub fn decide(intent: Intent, holds: Holds) -> Decision {
    if intent == Intent::Pause {
        return Decision::Paused;
    }
    if holds.network {
        Decision::Held(HoldReason::Network)
    } else if holds.schedule {
        Decision::Held(HoldReason::Schedule)
    } else if holds.ratio {
        Decision::Held(HoldReason::Ratio)
    } else if holds.queue {
        Decision::Held(HoldReason::Queue)
    } else {
        Decision::Run
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runs_when_nothing_holds_it() {
        assert_eq!(decide(Intent::Run, Holds::default()), Decision::Run);
    }

    #[test]
    fn user_pause_wins_over_everything() {
        let all = Holds {
            network: true,
            schedule: true,
            ratio: true,
            queue: true,
        };
        assert_eq!(decide(Intent::Pause, Holds::default()), Decision::Paused);
        assert_eq!(decide(Intent::Pause, all), Decision::Paused);
    }

    #[test]
    fn any_single_hold_stops_it() {
        for holds in [
            Holds {
                network: true,
                ..Default::default()
            },
            Holds {
                schedule: true,
                ..Default::default()
            },
            Holds {
                ratio: true,
                ..Default::default()
            },
            Holds {
                queue: true,
                ..Default::default()
            },
        ] {
            assert!(matches!(decide(Intent::Run, holds), Decision::Held(_)));
        }
    }

    #[test]
    fn releasing_one_hold_keeps_the_others() {
        let holds = Holds {
            schedule: true,
            queue: true,
            ..Default::default()
        };
        assert_eq!(decide(Intent::Run, holds), Decision::Held(HoldReason::Schedule));
        let holds = Holds {
            schedule: false,
            ..holds
        };
        assert_eq!(decide(Intent::Run, holds), Decision::Held(HoldReason::Queue));
    }

    #[test]
    fn network_is_reported_first() {
        let all = Holds {
            network: true,
            schedule: true,
            ratio: true,
            queue: true,
        };
        assert_eq!(decide(Intent::Run, all), Decision::Held(HoldReason::Network));
    }
}
