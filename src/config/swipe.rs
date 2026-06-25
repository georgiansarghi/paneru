use serde::Deserialize;

use crate::{config::deserialize_modifier, platform::Modifiers};

#[derive(Clone, Debug, Deserialize)]
pub enum SwipeGestureDirection {
    Natural,
    Reversed,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub enum SwipeGestureHorizontalAction {
    #[serde(alias = "scroll")]
    Scroll,
    #[serde(alias = "focus")]
    Focus,
    #[serde(alias = "disabled")]
    Disabled,
}

#[derive(Deserialize, Clone, Debug)]
#[serde(untagged)]
pub enum SwipeGestureHorizontalConfig {
    /// Backward-compatible shorthand: `horizontal = "scroll"` or `"focus"`.
    Action(SwipeGestureHorizontalAction),
    /// Per-finger-count routing: `[[swipe.gesture.horizontal]]` tables.
    Rules(Vec<SwipeGestureHorizontalRule>),
}

#[derive(Deserialize, Clone, Debug)]
pub struct SwipeGestureHorizontalRule {
    /// Number of fingers that should trigger this horizontal gesture rule.
    pub fingers_count: usize,

    /// Action to perform for this finger count.
    pub action: SwipeGestureHorizontalAction,

    /// Gesture-specific sensitivity multiplier. Higher values require a
    /// shorter swipe. Multiplies `[swipe].sensitivity` for threshold checks.
    pub sensitivity: Option<f64>,

    /// Explicit accumulated normalized delta required to fire the gesture.
    /// Lower values are more sensitive. Overrides `sensitivity` when set.
    pub threshold: Option<f64>,
}

#[derive(Deserialize, Clone, Debug, Default)]
pub struct SwipeOptions {
    /// Swipe sensitivity multiplier. Lower values = less distance per finger
    /// movement. Range: 0.1–2.0. Default: 0.35.
    pub sensitivity: Option<f64>,

    /// Swipe inertia deceleration rate. Higher values = faster stop.
    /// Range: 1.0–10.0. Default: 4.0.
    pub deceleration: Option<f64>,

    /// Swiping keeps sliding windows until the first or last window.
    /// Set to false to clamp so edge windows stay on-screen. Default: true.
    #[allow(dead_code)]
    pub continuous: Option<bool>,

    pub gesture: Option<GestureOptions>,
    pub scroll: Option<ScrollOptions>,
}

#[derive(Deserialize, Clone, Debug, Default)]
pub struct GestureOptions {
    /// The number of fingers required for swipe gestures to move windows.
    pub fingers_count: Option<usize>,

    /// Which direction swipe gestures should move windows.
    pub direction: Option<SwipeGestureDirection>,

    /// How horizontal swipe gestures should behave.
    pub horizontal: Option<SwipeGestureHorizontalConfig>,

    /// Explicit accumulated normalized delta required to fire one-shot
    /// gestures. Lower values are more sensitive.
    pub threshold: Option<f64>,

    /// Whether to intercept vertical swipes.
    pub vertical: Option<bool>,
}

#[derive(Deserialize, Clone, Debug, Default)]
pub struct ScrollOptions {
    /// Modifier key(s) required for scroll wheel swiping.
    /// Accepts the same format as keybindings: "alt", "cmd", "alt + cmd", "alt + rcmd" etc.
    #[serde(default, deserialize_with = "deserialize_modifier")]
    pub modifier: Option<Modifiers>,

    /// Additional modifier key(s) that, combined with the scroll modifier,
    /// switches virtual workspaces vertically instead of scrolling horizontally.
    #[serde(default, deserialize_with = "deserialize_modifier")]
    pub vertical_modifier: Option<Modifiers>,
}
