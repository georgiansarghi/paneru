use std::time::Duration;

use super::adaptive_quiet_idle_cap_ms;

/// Pure policy for deciding when Paneru's future top-level runtime driver should
/// run Bevy again, wait, or exit.
///
/// This module intentionally does not touch `AppKit`, `Bevy`, sockets, or wall-clock
/// time. Production loop behavior is unchanged; these types are a deterministic
/// model that later runner tickets can wire to real sources.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RuntimeDriverPolicy {
    config: RuntimeDriverConfig,
}

impl RuntimeDriverPolicy {
    #[must_use]
    pub const fn new(config: RuntimeDriverConfig) -> Self {
        Self { config }
    }

    /// Returns the next runner action for a snapshot of runtime state.
    #[must_use]
    pub fn decide(&self, state: RuntimeDriverState) -> RunnerDecision {
        if state.shutdown_requested {
            return RunnerDecision::Exit;
        }
        if state.external_event_pending {
            return RunnerDecision::RunUpdateNow(UpdateReason::ExternalEvent);
        }
        if state.bevy_dirty {
            return RunnerDecision::RunUpdateNow(UpdateReason::BevyDirty);
        }
        if let Some(deadline) = state.next_visible_deadline
            && deadline.at <= state.now
        {
            return RunnerDecision::RunUpdateNow(UpdateReason::DeadlineElapsed(deadline.reason));
        }

        let mut wait = if state.active_work.frame_active() {
            WaitDeadline {
                duration: self.config.frame_interval,
                reason: DeadlineReason::AnimationFrame,
            }
        } else if state.recent_interactive_activity(self.config.interactive_grace_period) {
            WaitDeadline {
                duration: self.config.interactive_idle_cap,
                reason: DeadlineReason::RecentInteractiveActivity,
            }
        } else if state.low_power {
            WaitDeadline {
                duration: self.config.low_power_watchdog_cap,
                reason: DeadlineReason::LowPowerWatchdog,
            }
        } else {
            WaitDeadline {
                duration: self.config.idle_watchdog_cap,
                reason: DeadlineReason::IdleWatchdog,
            }
        };

        if let Some(deadline) = state.next_visible_deadline {
            let until_deadline = deadline.at.saturating_sub(state.now);
            if until_deadline < wait.duration {
                wait = WaitDeadline {
                    duration: until_deadline,
                    reason: deadline.reason,
                };
            }
        }

        RunnerDecision::Wait(wait)
    }
}

impl Default for RuntimeDriverPolicy {
    fn default() -> Self {
        Self::new(RuntimeDriverConfig::legacy_equivalent())
    }
}

/// Timing knobs for the pure runner policy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RuntimeDriverConfig {
    pub frame_interval: Duration,
    pub interactive_idle_cap: Duration,
    pub idle_watchdog_cap: Duration,
    pub low_power_watchdog_cap: Duration,
    pub interactive_grace_period: Duration,
}

impl RuntimeDriverConfig {
    /// Legacy-equivalent caps matching the currently accepted responsive loop.
    #[must_use]
    pub const fn legacy_equivalent() -> Self {
        Self {
            frame_interval: Duration::from_millis(16),
            interactive_idle_cap: Duration::from_millis(50),
            idle_watchdog_cap: Duration::from_millis(50),
            low_power_watchdog_cap: Duration::from_millis(500),
            interactive_grace_period: Duration::from_secs(1),
        }
    }

    pub(crate) const fn production(legacy_idle_cadence_forced: bool) -> Self {
        Self {
            frame_interval: Duration::from_millis(16),
            interactive_idle_cap: Duration::from_millis(50),
            idle_watchdog_cap: if legacy_idle_cadence_forced {
                Duration::from_millis(50)
            } else {
                Duration::from_millis(adaptive_quiet_idle_cap_ms() as u64)
            },
            low_power_watchdog_cap: Duration::from_millis(500),
            interactive_grace_period: Duration::from_secs(1),
        }
    }
}

impl Default for RuntimeDriverConfig {
    fn default() -> Self {
        Self::legacy_equivalent()
    }
}

/// A deterministic snapshot of the inputs relevant to runner scheduling.
#[allow(clippy::struct_excessive_bools)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RuntimeDriverState {
    pub now: Duration,
    pub external_event_pending: bool,
    pub bevy_dirty: bool,
    pub active_work: ActiveRuntimeWork,
    pub last_interactive_activity: Option<Duration>,
    pub next_visible_deadline: Option<RuntimeDeadline>,
    pub low_power: bool,
    pub shutdown_requested: bool,
}

impl RuntimeDriverState {
    #[must_use]
    pub fn recent_interactive_activity(self, grace_period: Duration) -> bool {
        self.last_interactive_activity
            .is_some_and(|last_activity| self.now.saturating_sub(last_activity) < grace_period)
    }
}

/// Work that should keep frame-rate deadlines short while it is visible.
#[allow(clippy::struct_excessive_bools)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ActiveRuntimeWork {
    pub repositioning: bool,
    pub resizing: bool,
    pub scrolling: bool,
    pub flash_message: bool,
}

impl ActiveRuntimeWork {
    #[must_use]
    pub const fn frame_active(self) -> bool {
        self.repositioning || self.resizing || self.scrolling || self.flash_message
    }
}

/// A runner-visible deadline such as a watchdog, timer, state save, or repair.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RuntimeDeadline {
    pub at: Duration,
    pub reason: DeadlineReason,
}

impl RuntimeDeadline {
    #[must_use]
    pub const fn new(at: Duration, reason: DeadlineReason) -> Self {
        Self { at, reason }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunnerDecision {
    RunUpdateNow(UpdateReason),
    Wait(WaitDeadline),
    Exit,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WaitDeadline {
    pub duration: Duration,
    pub reason: DeadlineReason,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UpdateReason {
    ExternalEvent,
    BevyDirty,
    DeadlineElapsed(DeadlineReason),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum DeadlineReason {
    AnimationFrame,
    RecentInteractiveActivity,
    IdleWatchdog,
    LowPowerWatchdog,
    PeriodicMaintenance,
    StateSave,
    LostFocusWatchdog,
    OrphanWorkspaceWatchdog,
    RefreshWindowSizes,
    NativeTabReconciliation,
    TimeoutComponent,
}

impl DeadlineReason {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::AnimationFrame => "animation_frame",
            Self::RecentInteractiveActivity => "recent_interactive_activity",
            Self::IdleWatchdog => "idle_watchdog",
            Self::LowPowerWatchdog => "low_power_watchdog",
            Self::PeriodicMaintenance => "periodic_maintenance",
            Self::StateSave => "state_save",
            Self::LostFocusWatchdog => "lost_focus_watchdog",
            Self::OrphanWorkspaceWatchdog => "orphan_workspace_watchdog",
            Self::RefreshWindowSizes => "refresh_window_sizes",
            Self::NativeTabReconciliation => "native_tab_reconciliation",
            Self::TimeoutComponent => "timeout_component",
        }
    }
}
