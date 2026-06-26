use std::collections::BTreeSet;
use std::time::Duration;

use bevy::ecs::resource::Resource;

/// Marker resource inserted when waiting is owned by Paneru's custom top-level
/// runner instead of the legacy `PreUpdate` pump system.
#[derive(Clone, Copy, Debug, Resource)]
pub struct RuntimeDriverActive;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum RuntimeActivityReason {
    ExternalEvent,
    Input,
    WindowEvent,
    Focus,
    Layout,
    Animation,
    Command,
}

impl RuntimeActivityReason {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::ExternalEvent => "external_event",
            Self::Input => "input",
            Self::WindowEvent => "window_event",
            Self::Focus => "focus",
            Self::Layout => "layout",
            Self::Animation => "animation",
            Self::Command => "command",
        }
    }
}

#[derive(Clone, Debug, Default, Resource)]
pub(crate) struct RuntimeActivity {
    last_activity: Option<Duration>,
    reasons: BTreeSet<RuntimeActivityReason>,
}

impl RuntimeActivity {
    pub(crate) fn mark(&mut self, now: Duration, reason: RuntimeActivityReason) {
        self.last_activity = Some(now);
        self.reasons.insert(reason);
    }

    pub(crate) fn recent(&self, now: Duration, grace: Duration) -> bool {
        self.last_activity
            .is_some_and(|last| now.saturating_sub(last) < grace)
    }

    pub(crate) const fn last_activity(&self) -> Option<Duration> {
        self.last_activity
    }

    pub(crate) fn reason_names(&self) -> Vec<&'static str> {
        self.reasons.iter().map(|reason| reason.as_str()).collect()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum RuntimeDirtyReason {
    CommandHandled,
    InternalMessageEmitted,
    FocusMarkerChanged,
    AnimationMarkerInserted,
}

impl RuntimeDirtyReason {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::CommandHandled => "command_handled",
            Self::InternalMessageEmitted => "internal_message_emitted",
            Self::FocusMarkerChanged => "focus_marker_changed",
            Self::AnimationMarkerInserted => "animation_marker_inserted",
        }
    }
}

/// Bevy-internal work that should make the custom runner perform another update
/// before it considers waiting. This covers perf-04 failure modes where work was
/// queued via Bevy messages/triggers rather than Paneru's external event queue.
#[derive(Clone, Debug, Default, Resource)]
pub(crate) struct RuntimeDirty {
    reasons: BTreeSet<RuntimeDirtyReason>,
}

impl RuntimeDirty {
    pub(crate) fn mark(&mut self, reason: RuntimeDirtyReason) {
        self.reasons.insert(reason);
    }

    pub(crate) fn take(&mut self) -> Vec<RuntimeDirtyReason> {
        std::mem::take(&mut self.reasons).into_iter().collect()
    }
}
