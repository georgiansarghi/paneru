use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use bevy::ecs::resource::Resource;

use super::{DeadlineReason, RuntimeDeadline, WaitDeadline};

#[derive(Clone, Debug, Resource)]
pub(crate) struct RuntimeDeadlineClock {
    pub(crate) started: Instant,
}

impl Default for RuntimeDeadlineClock {
    fn default() -> Self {
        Self {
            started: Instant::now(),
        }
    }
}

impl RuntimeDeadlineClock {
    pub(crate) fn now(&self) -> Duration {
        self.started.elapsed()
    }
}

/// Runner-visible deadline registry. Deadlines are stored against the registry
/// clock rather than hidden inside Bevy `on_timer` run conditions, so future
/// deep-idle policy can choose a safe wake without knowing each system's
/// internals.
#[derive(Clone, Debug, Default, Resource)]
pub(crate) struct RuntimeDeadlines {
    pub(crate) deadlines: BTreeMap<DeadlineReason, Duration>,
}

impl RuntimeDeadlines {
    pub(crate) fn set_after(&mut self, now: Duration, reason: DeadlineReason, after: Duration) {
        self.deadlines.insert(reason, now + after);
    }

    pub(crate) fn clear(&mut self, reason: DeadlineReason) {
        self.deadlines.remove(&reason);
    }

    pub(crate) fn due(&self, now: Duration, reason: DeadlineReason) -> bool {
        self.deadlines
            .get(&reason)
            .is_some_and(|deadline| *deadline <= now)
    }

    pub(crate) fn deadline_at(&self, reason: DeadlineReason) -> Option<Duration> {
        self.deadlines.get(&reason).copied()
    }

    pub(crate) fn due_reasons(&self, now: Duration) -> Vec<DeadlineReason> {
        self.deadlines
            .iter()
            .filter_map(|(reason, deadline)| (*deadline <= now).then_some(*reason))
            .collect()
    }

    pub(crate) fn ensure_repeating_after(
        &mut self,
        now: Duration,
        reason: DeadlineReason,
        period: Duration,
    ) -> bool {
        if self
            .deadlines
            .get(&reason)
            .is_none_or(|deadline| *deadline <= now)
        {
            self.set_after(now, reason, period);
            return true;
        }
        false
    }

    pub(crate) fn earliest(&self) -> Option<RuntimeDeadline> {
        self.deadlines
            .iter()
            .min_by_key(|(_, deadline)| **deadline)
            .map(|(reason, at)| RuntimeDeadline::new(*at, *reason))
    }

    pub(crate) fn earliest_wait(&self, now: Duration) -> Option<WaitDeadline> {
        self.earliest().map(|deadline| WaitDeadline {
            duration: deadline.at.saturating_sub(now),
            reason: deadline.reason,
        })
    }
}
