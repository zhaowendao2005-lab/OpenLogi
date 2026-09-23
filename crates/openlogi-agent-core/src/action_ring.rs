//! Agent-owned Actions Ring invocation and selection state.
//!
//! The overlay receives an opaque session and a read-only presentation
//! snapshot. Executable actions remain in the agent, and IPC commands can
//! select only a slot from that authoritative snapshot.

use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use openlogi_core::action_ring::DISPLAY_LIFETIME;
use openlogi_core::binding::{Action, ActionRingIcon, ActionRingLayout, ActionRingSlot};
use openlogi_hid::DeviceRoute;
use openlogi_ipc::{
    ActionRingCommandError, ActionRingInvocation, ActionRingPresentation, AppWindowsPresentation,
    Generation, OBSERVE_HOLD, RingObservation,
};
use tokio::sync::watch;

/// Slack between the window closing and the session expiring.
///
/// The two clocks do not start together: a session is stamped in `begin`,
/// while the overlay's timer starts only once the long poll has delivered the
/// invocation and the window is up. Giving them equal durations expired the
/// session *before* the ring the user could still click disappeared, and a
/// click landing in that tail returned `SessionNotFound` — the window closed
/// and the action silently did not run. This covers that gap plus the click's
/// round-trip back.
const SESSION_GRACE: Duration = Duration::from_secs(3);

const SESSION_LIFETIME: Duration = DISPLAY_LIFETIME.saturating_add(SESSION_GRACE);
const WINDOW_CYCLE_BURST: Duration = Duration::from_millis(900);


/// Immutable input used to open one ring session.
pub struct ActionRingSessionSpec {
    /// Foreground target captured when the physical trigger arrived.
    pub target: crate::runtime::ActionDispatchTarget,
    /// Device-wide appearance and dismissal playback speed.
    pub animation_speed: openlogi_core::binding::ActionRingAnimationSpeed,
    /// Config key of the device whose control opened the ring.
    pub device_key: String,
    /// HID++ route used for feedback when both config and capabilities allow it.
    pub haptic_route: Option<DeviceRoute>,
    /// Exact layout the agent will execute for this session.
    pub layout: ActionRingLayout,
    /// Configured UI locale, or `None` to follow the overlay host's system locale.
    pub language: Option<String>,
}

/// Common presentation settings for an application-window session.
pub struct WindowSessionSpec {
    /// Foreground target captured at physical button-down.
    pub target: crate::runtime::ActionDispatchTarget,
    /// Device whose binding opened the session.
    pub device_key: String,
    /// Playback speed used by the overlay.
    pub animation_speed: openlogi_core::binding::ActionRingAnimationSpeed,
    /// Configured UI locale.
    pub language: Option<String>,
}

/// A validated slot activation returned to the action dispatcher.
pub struct ActionRingActivation {
    /// Foreground target captured when the physical trigger arrived.
    pub target: crate::runtime::ActionDispatchTarget,
    /// Config key of the device whose control opened the ring.
    pub device_key: String,
    /// Action snapshotted when the ring opened.
    pub action: Action,
    /// Route of the triggering device when activation feedback is available.
    pub haptic_route: Option<DeviceRoute>,
}

/// A validated hover transition that may play feedback.
#[derive(Debug, PartialEq, Eq)]
pub struct ActionRingHover {
    /// Route of the triggering device when hover feedback is available.
    pub haptic_route: Option<DeviceRoute>,
}

struct Session {
    invocation: ActionRingInvocation,
    device_key: String,
    haptic_route: Option<DeviceRoute>,
    target: crate::runtime::ActionDispatchTarget,
    actions: BTreeMap<ActionRingSlot, Action>,
    /// Native targets retained only in the agent. The index matches the
    /// presentation title and is never serialized across IPC.
    window_targets: Vec<openlogi_inject::app_windows::WindowTarget>,
    hovered: Option<ActionRingSlot>,
    opened_at: Instant,
    input_region: Option<(openlogi_core::action_ring::InputRegion, Instant)>,
}

impl Session {
    fn has_content(&self) -> bool {
        !self.actions.is_empty()
            || !self.window_targets.is_empty()
            || self.invocation.windows.is_some()
    }

    fn live_input_region(&self, now: Instant) -> Option<openlogi_core::action_ring::InputRegion> {
        let (region, renewed) = self.input_region?;
        (now.saturating_duration_since(self.opened_at) <= SESSION_LIFETIME
            && now.saturating_duration_since(renewed) < openlogi_core::action_ring::INPUT_LEASE)
            .then_some(region)
    }
}

struct WindowCycle {
    executable: String,
    windows: Vec<openlogi_inject::app_windows::AppWindow>,
    current: (usize, u32),
    last_press: Instant,
}

#[derive(Default)]
struct State {
    active: Option<Session>,
    /// Access order is kept per executable path, never by process id. A
    /// process restart must not make a new application inherit another one's
    /// window history.
    histories: HashMap<String, Vec<openlogi_inject::app_windows::AppWindow>>,
    /// A repeated short-press burst uses this immutable ordering. Foreground
    /// observations can update `histories` without making A/B oscillate into
    /// A/B when the user presses rapidly.
    cycle: Option<WindowCycle>,
}

impl State {
    fn expire(&mut self) {
        if self
            .active
            .as_ref()
            .is_some_and(|session| session.opened_at.elapsed() > SESSION_LIFETIME)
        {
            self.active = None;
        }
    }

    fn active_session(&mut self, session_id: u64) -> Result<&mut Session, ActionRingCommandError> {
        self.expire();
        match self.active.as_mut() {
            Some(session) if session.invocation.session_id == session_id => Ok(session),
            _ => Err(ActionRingCommandError::SessionNotFound),
        }
    }

    fn merge_history(
        &mut self,
        windows: &[openlogi_inject::app_windows::AppWindow],
    ) -> Vec<openlogi_inject::app_windows::AppWindow> {
        let Some(first) = windows.first() else {
            return Vec::new();
        };
        let executable = first.executable.clone();
        let mut current = windows
            .iter()
            .cloned()
            .map(|window| (window.target.identity(), window))
            .collect::<HashMap<_, _>>();
        let previous = self.histories.remove(&executable).unwrap_or_default();
        let mut ordered = Vec::with_capacity(current.len());
        for window in previous {
            if let Some(updated) = current.remove(&window.target.identity()) {
                ordered.push(updated);
            }
        }
        for window in windows {
            if let Some(new_window) = current.remove(&window.target.identity()) {
                ordered.push(new_window);
            }
        }
        self.histories.insert(executable, ordered.clone());
        ordered
    }
}

#[cfg(target_os = "windows")]
fn window_target(
    target: crate::runtime::ActionDispatchTarget,
) -> Option<openlogi_inject::app_windows::WindowTarget> {
    match target {
        crate::runtime::ActionDispatchTarget::WindowsWindow(window) => Some(
            openlogi_inject::app_windows::WindowTarget::from_foreground(window),
        ),
        _ => None,
    }
}

#[cfg(not(target_os = "windows"))]
fn window_target(
    _target: crate::runtime::ActionDispatchTarget,
) -> Option<openlogi_inject::app_windows::WindowTarget> {
    None
}

fn refresh_cycle_order(
    frozen: Vec<openlogi_inject::app_windows::AppWindow>,
    history: &[openlogi_inject::app_windows::AppWindow],
) -> Vec<openlogi_inject::app_windows::AppWindow> {
    let mut live = history
        .iter()
        .cloned()
        .map(|window| (window.target.identity(), window))
        .collect::<HashMap<_, _>>();
    let mut order = Vec::with_capacity(live.len());
    for window in frozen {
        if let Some(updated) = live.remove(&window.target.identity()) {
            order.push(updated);
        }
    }
    // New windows appear at the end, preserving the frozen order of every
    // window that existed when the burst started.
    for window in history {
        if let Some(new_window) = live.remove(&window.target.identity()) {
            order.push(new_window);
        }
    }
    order
}

/// Shared ring state used by input dispatch and IPC handlers.
pub struct ActionRingManager {
    next_session: AtomicU64,
    state: Mutex<State>,
    /// What the overlay observes. Derived from [`Self::state`] after every
    /// change, so "no ring" is simply the absence of one — there is no closed
    /// message to invent, and an overlay that restarts mid-ring reads the live
    /// one instead of having missed its invocation.
    published: watch::Sender<RingObservation>,
}

impl Default for ActionRingManager {
    fn default() -> Self {
        // Generation 1: 0 is the observer's "seen nothing" sentinel.
        let (published, _) = watch::channel(RingObservation {
            generation: 1,
            invocation: None,
        });
        Self {
            next_session: AtomicU64::new(1),
            state: Mutex::new(State::default()),
            published,
        }
    }
}

impl ActionRingManager {
    /// Open or replace the current session and wake the overlay long-poll.
    pub fn begin(&self, spec: ActionRingSessionSpec) -> ActionRingInvocation {
        let session_id = self.next_session.fetch_add(1, Ordering::Relaxed);
        let mut actions = BTreeMap::new();
        let mut slots = BTreeMap::new();
        for (slot, entry) in spec.layout.slots {
            let (action, custom_icon, custom_label) = entry.into_parts();
            let literal = custom_label.is_some();
            slots.insert(
                slot,
                ActionRingPresentation {
                    label: custom_label.unwrap_or_else(|| action.label()),
                    literal,
                    icon: custom_icon.unwrap_or_else(|| ActionRingIcon::for_action(&action)),
                },
            );
            actions.insert(slot, action);
        }
        let invocation = ActionRingInvocation {
            animation_speed: spec.animation_speed,
            session_id,
            slots,
            language: spec.language,
            windows: None,
        };
        let mut state = self.state();
        state.active = Some(Session {
            invocation: invocation.clone(),
            device_key: spec.device_key,
            target: spec.target,
            haptic_route: spec.haptic_route,
            actions,
            window_targets: Vec::new(),
            hovered: None,
            opened_at: Instant::now(),
            input_region: None,
        });
        self.publish(&state);
        invocation
    }

    /// Cycle foreground windows silently on short press without opening any overlay session.
    pub fn cycle_window_next(&self, target: crate::runtime::ActionDispatchTarget) -> bool {
        let Some(target) = window_target(target) else {
            tracing::warn!("application window switching is not supported on this platform");
            return false;
        };
        if target.is_openlogi_overlay() {
            tracing::debug!("OpenLogi overlay windows are not switchable");
            return false;
        }
        let windows = match openlogi_inject::app_windows::list(target) {
            Ok(windows) if !windows.is_empty() => windows,
            Ok(_) => {
                tracing::warn!("no top-level windows found for the foreground application");
                return false;
            }
            Err(error) => {
                tracing::warn!(%error, "failed to list application windows for cycle");
                return false;
            }
        };
        let current = target.identity();
        let now = Instant::now();
        let (_order, selected_window, selected_target) = {
            let mut state = self.state();
            let history = state.merge_history(&windows);
            let executable = windows[0].executable.clone();
            let use_frozen = state.cycle.as_ref().is_some_and(|cycle| {
                cycle.executable == executable
                    && now.saturating_duration_since(cycle.last_press) <= WINDOW_CYCLE_BURST
                    && cycle
                        .windows
                        .iter()
                        .any(|window| window.target.identity() == current)
            });
            let order = if use_frozen {
                refresh_cycle_order(
                    state
                        .cycle
                        .as_ref()
                        .map_or_else(Vec::new, |cycle| cycle.windows.clone()),
                    &history,
                )
            } else {
                history
            };
            let selected = order
                .iter()
                .position(|window| window.target.identity() == current)
                .map_or(0, |current_index| (current_index + 1) % order.len());
            let selected_window = order[selected].clone();
            let selected_target = selected_window.target;
            state.cycle = Some(WindowCycle {
                executable: executable.clone(),
                windows: order.clone(),
                current: selected_target.identity(),
                last_press: now,
            });
            (order, selected_window, selected_target)
        };
        let activated = selected_target == target || selected_target.activate();
        if activated {
            self.record_window_visit(&selected_window);
        }
        activated
    }

    /// Open an interactive window-selection ring. Its native targets stay in
    /// the agent session; the overlay receives only titles and slot indices.
    pub fn begin_window_selection(&self, spec: WindowSessionSpec) -> ActionRingInvocation {
        let Some(target) = window_target(spec.target) else {
            return self.begin_window_error(
                spec,
                "application window switching is not supported on this platform",
            );
        };
        if target.is_openlogi_overlay() {
            return self.begin_window_error(spec, "OpenLogi overlay windows are not switchable");
        }
        let windows = match openlogi_inject::app_windows::list(target) {
            Ok(windows) if !windows.is_empty() => windows,
            Ok(_) => {
                return self.begin_window_error(
                    spec,
                    "no top-level windows found for the foreground application",
                );
            }
            Err(error) => return self.begin_window_error(spec, &error.to_string()),
        };
        let current = target.identity();
        let (order, selected) = {
            let mut state = self.state();
            let order = state.merge_history(&windows);
            state.cycle = None;
            let selected = order
                .iter()
                .position(|window| window.target.identity() == current)
                .unwrap_or(0);
            (order, selected)
        };
        self.begin_window_session(spec, order, Some(selected), None)
    }

    /// Record the real foreground application after an external activation.
    /// Overlay windows are explicitly ignored so opening the picker cannot
    /// pollute application MRU history.
    pub fn observe_foreground(&self) {
        let Some(target) = openlogi_inject::app_windows::WindowTarget::capture() else {
            return;
        };
        if target.is_openlogi_overlay() {
            return;
        }
        let Ok(windows) = openlogi_inject::app_windows::list(target) else {
            return;
        };
        let Some(first) = windows.first() else {
            return;
        };
        let identity = target.identity();
        let mut state = self.state();
        if state
            .cycle
            .as_ref()
            .is_some_and(|cycle| cycle.executable != first.executable || cycle.current != identity)
        {
            state.cycle = None;
        }
        let mut history = state.merge_history(&windows);
        if let Some(position) = history
            .iter()
            .position(|window| window.target.identity() == identity)
        {
            let current = history.remove(position);
            history.insert(0, current);
            state.histories.insert(first.executable.clone(), history);
        }
    }

    fn record_window_visit(&self, window: &openlogi_inject::app_windows::AppWindow) {
        let mut state = self.state();
        let Some(history) = state.histories.get_mut(&window.executable) else {
            return;
        };
        let Some(position) = history
            .iter()
            .position(|candidate| candidate.target.identity() == window.target.identity())
        else {
            return;
        };
        let current = history.remove(position);
        history.insert(0, current);
    }

    /// Activate the native target represented by a picker slot. `None` means
    /// the active session is an ordinary Actions Ring and its action path
    /// should handle the same slot.
    pub fn activate_window(
        &self,
        session_id: u64,
        slot: ActionRingSlot,
    ) -> Result<Option<openlogi_inject::app_windows::WindowTarget>, ActionRingCommandError> {
        let mut state = self.state();
        let session = state.active_session(session_id)?;
        if session.window_targets.is_empty() {
            return Ok(None);
        }
        let index = slot.index();
        let Some(target) = session.window_targets.get(index).copied() else {
            return Err(ActionRingCommandError::SlotEmpty);
        };
        state.active = None;
        self.publish(&state);
        Ok(Some(target))
    }

    fn begin_window_error(&self, spec: WindowSessionSpec, error: &str) -> ActionRingInvocation {
        self.begin_window_session(spec, Vec::new(), None, Some(error.to_string()))
    }

    fn begin_window_session(
        &self,
        spec: WindowSessionSpec,
        mut windows: Vec<openlogi_inject::app_windows::AppWindow>,
        selected: Option<usize>,
        error: Option<String>,
    ) -> ActionRingInvocation {
        let limit = ActionRingSlot::ALL.len();
        windows.truncate(limit);
        let selected = selected
            .filter(|index| *index < windows.len())
            .and_then(|index| u32::try_from(index).ok());
        let titles = windows
            .iter()
            .map(|window| window.title.clone())
            .collect::<Vec<_>>();
        let mut slots = BTreeMap::new();
        for (index, window) in windows.iter().enumerate() {
            slots.insert(
                ActionRingSlot::ALL[index],
                ActionRingPresentation {
                    label: window.title.clone(),
                    literal: true,
                    icon: ActionRingIcon::Monitor,
                },
            );
        }
        if slots.is_empty()
            && let Some(error) = error.as_ref()
        {
            slots.insert(
                ActionRingSlot::Top,
                ActionRingPresentation {
                    label: error.clone(),
                    literal: true,
                    icon: ActionRingIcon::Ban,
                },
            );
        }
        let session_id = self.next_session.fetch_add(1, Ordering::Relaxed);
        let invocation = ActionRingInvocation {
            animation_speed: spec.animation_speed,
            session_id,
            slots,
            language: spec.language,
            windows: Some(AppWindowsPresentation {
                titles,
                selected,
                feedback: false,
                error,
            }),
        };
        let targets = windows.iter().map(|window| window.target).collect();
        let mut state = self.state();
        state.active = Some(Session {
            invocation: invocation.clone(),
            device_key: spec.device_key,
            haptic_route: None,
            target: spec.target,
            actions: BTreeMap::new(),
            window_targets: targets,
            hovered: None,
            opened_at: Instant::now(),
            input_region: None,
        });
        self.publish(&state);
        invocation
    }

    /// Dismiss the showing session, if any, and return whether one was
    /// dismissed — which is what lets a second press of the ring trigger toggle
    /// the ring closed.
    ///
    /// Dismissing simply removes the session. The old wire format could only
    /// carry invocations, so a dismissal had to be encoded as an *empty*
    /// invocation for the overlay to acknowledge; observing state needs no such
    /// placeholder, and a trigger press racing the close now finds no session
    /// and opens a fresh ring.
    pub fn dismiss_active(&self) -> bool {
        let mut state = self.state();
        state.expire();
        let dismissed = matches!(&state.active, Some(session) if session.has_content());
        if dismissed {
            state.active = None;
        }
        self.publish(&state);
        dismissed
    }

    /// Serve one [`Agent::observe_action_ring`](openlogi_ipc::Agent::observe_action_ring).
    pub async fn observe(&self, since: Generation) -> RingObservation {
        let mut rx = self.published.subscribe();
        let changed = rx.wait_for(|observed| observed.generation != since);
        match tokio::time::timeout(OBSERVE_HOLD, changed).await {
            Ok(Ok(observed)) => observed.clone(),
            // Hold elapsed, or the manager is gone: answer with what we have.
            Ok(Err(_)) | Err(_) => self.published.borrow().clone(),
        }
    }

    /// Republish what the overlay should be showing. Called after every change
    /// to [`Self::state`], and a republish that says the same thing wakes
    /// nobody.
    fn publish(&self, state: &State) {
        let invocation = state
            .active
            .as_ref()
            .map(|session| session.invocation.clone());
        self.published.send_if_modified(|observed| {
            if observed.invocation == invocation {
                return false;
            }
            observed.invocation = invocation;
            observed.generation += 1;
            true
        });
    }

    /// Record a changed highlighted slot. Repeated hover reports are ignored so
    /// one stationary pointer cannot flood the HID++ haptic queue.
    pub fn hover(
        &self,
        session_id: u64,
        slot: ActionRingSlot,
    ) -> Result<Option<ActionRingHover>, ActionRingCommandError> {
        let mut state = self.state();
        // `active_session` expires a stale session, which the overlay must hear
        // about even though the hover itself then fails.
        let session = match state.active_session(session_id) {
            Ok(session) => session,
            Err(error) => {
                self.publish(&state);
                return Err(error);
            }
        };
        if !session.actions.contains_key(&slot)
            && session.window_targets.get(slot.index()).is_none()
        {
            return Err(ActionRingCommandError::SlotEmpty);
        }
        if session.hovered == Some(slot) {
            return Ok(None);
        }
        session.hovered = Some(slot);
        Ok(Some(ActionRingHover {
            haptic_route: session.haptic_route.clone(),
        }))
    }

    /// Consume a session and return the snapshotted action for `slot`.
    pub fn activate(
        &self,
        session_id: u64,
        slot: ActionRingSlot,
    ) -> Result<ActionRingActivation, ActionRingCommandError> {
        let mut state = self.state();
        if !state
            .active_session(session_id)?
            .actions
            .contains_key(&slot)
        {
            return Err(ActionRingCommandError::SlotEmpty);
        }
        let Some(mut session) = state.active.take() else {
            return Err(ActionRingCommandError::SessionNotFound);
        };
        self.publish(&state);
        let Some(action) = session.actions.remove(&slot) else {
            return Err(ActionRingCommandError::SlotEmpty);
        };
        Ok(ActionRingActivation {
            target: session.target,
            device_key: session.device_key,
            action,
            haptic_route: session.haptic_route,
        })
    }

    /// Cancel `session_id` if it is still active.
    pub fn cancel(&self, session_id: u64) {
        let mut state = self.state();
        if state
            .active
            .as_ref()
            .is_some_and(|session| session.invocation.session_id == session_id)
        {
            state.active = None;
        }
        self.publish(&state);
    }

    /// Renew only the showing session's input grant. Invalid geometry disarms it.
    pub fn set_input_region(
        &self,
        session_id: u64,
        region: Option<openlogi_core::action_ring::InputRegion>,
    ) -> bool {
        let mut state = self.state();
        let Ok(session) = state.active_session(session_id) else {
            return false;
        };
        session.input_region = region.filter(|r| r.is_valid()).map(|r| (r, Instant::now()));
        true
    }

    /// Nonblocking input-hook decision. Missing, stale or contended state fails open.
    pub fn outside_session(&self, x: f64, y: f64) -> Option<u64> {
        self.outside_session_at(x, y, Instant::now())
    }

    fn outside_session_at(&self, x: f64, y: f64, now: Instant) -> Option<u64> {
        if !x.is_finite() || !y.is_finite() {
            return None;
        }
        let state = self.state.try_lock().ok()?;
        let session = state.active.as_ref()?;
        let region = session.live_input_region(now)?;
        if region.contains(x, y) {
            return None;
        }
        Some(session.invocation.session_id)
    }

    /// Keep native vertical wheel events available to the ring instead of smoothing them.
    pub fn wants_native_scroll(&self) -> bool {
        self.state
            .try_lock()
            .ok()
            .and_then(|state| {
                state
                    .active
                    .as_ref()
                    .and_then(|session| session.live_input_region(Instant::now()))
            })
            .is_some()
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use openlogi_core::binding::ActionRingConfig;

    fn spec() -> ActionRingSessionSpec {
        ActionRingSessionSpec {
            target: crate::runtime::ActionDispatchTarget::Keyboard,
            animation_speed: openlogi_core::binding::ActionRingAnimationSpeed::default(),
            device_key: "mouse-a".to_string(),
            haptic_route: None,
            layout: ActionRingConfig::default().default,
            language: None,
        }
    }

    #[test]
    fn unsupported_window_switch_is_explicit_and_session_scoped() {
        let manager = ActionRingManager::default();
        let invocation = manager.begin_window_selection(WindowSessionSpec {
            target: crate::runtime::ActionDispatchTarget::Keyboard,
            device_key: "mouse-a".to_string(),
            animation_speed: openlogi_core::binding::ActionRingAnimationSpeed::default(),
            language: None,
        });
        let windows = invocation.windows.expect("unsupported result is presented");
        assert!(!windows.feedback);
        let error = windows.error.expect("unsupported result has a reason");
        assert!(error.contains("not supported"));
        assert_eq!(invocation.slots[&ActionRingSlot::Top].label, error);
    }

    #[tokio::test]
    async fn an_overlay_that_has_seen_nothing_gets_the_showing_ring() {
        let manager = ActionRingManager::default();
        let expected = manager.begin(spec());
        // Generation 0 is "seen nothing", so this answers at once — which is
        // also how an overlay restarted mid-ring picks up the live one.
        let observed = manager.observe(0).await;
        assert_eq!(observed.invocation, Some(expected));
    }

    #[test]
    fn invocation_contains_presentation_but_not_execution_payloads() {
        let manager = ActionRingManager::default();
        let mut spec = spec();
        spec.layout
            .set_icon(ActionRingSlot::Top, Some(ActionRingIcon::Keyboard));
        spec.language = Some("fr".to_string());
        let invocation = manager.begin(spec);
        assert_eq!(
            invocation.slots[&ActionRingSlot::Top],
            ActionRingPresentation {
                label: "Cut".to_string(),
                literal: false,
                icon: ActionRingIcon::Keyboard,
            }
        );
        assert_eq!(invocation.language.as_deref(), Some("fr"));
    }

    #[tokio::test]
    async fn second_trigger_press_dismisses_and_third_reopens() {
        let manager = ActionRingManager::default();

        // Nothing showing yet: the first press must open, not dismiss.
        assert!(!manager.dismiss_active());
        let opened = manager.begin(spec());
        let showing = manager.observe(0).await;
        assert_eq!(showing.invocation, Some(opened.clone()));

        // Second press: dismissed, and the overlay observes "no ring" rather
        // than a placeholder invocation it has to acknowledge.
        assert!(manager.dismiss_active());
        let dismissed = manager.observe(showing.generation).await;
        assert_eq!(dismissed.invocation, None);

        // Nothing is showing, so a third press opens again.
        assert!(!manager.dismiss_active());
        let reopened = manager.begin(spec());
        assert!(!reopened.slots.is_empty());

        // A stale Cancel for the dismissed session must not kill the new one.
        manager.cancel(opened.session_id);
        assert!(manager.dismiss_active());
    }

    #[test]
    fn custom_slot_labels_override_the_action_label() {
        let manager = ActionRingManager::default();
        let mut spec = spec();
        spec.layout
            .set_label(ActionRingSlot::Top, Some("Copy Invoice".to_string()));
        let invocation = manager.begin(spec);
        assert_eq!(invocation.slots[&ActionRingSlot::Top].label, "Copy Invoice");
        // Custom labels are literal so the overlay renders them verbatim even
        // when they collide with a localization key.
        assert!(invocation.slots[&ActionRingSlot::Top].literal);
    }

    #[test]
    fn activation_consumes_the_session() {
        let manager = ActionRingManager::default();
        let invocation = manager.begin(spec());
        let activation = manager
            .activate(invocation.session_id, ActionRingSlot::Top)
            .expect("a live session must activate its top slot");
        assert_eq!(activation.device_key, "mouse-a");
        assert_eq!(activation.action, Action::Cut);
        assert!(matches!(
            manager.activate(invocation.session_id, ActionRingSlot::Top),
            Err(ActionRingCommandError::SessionNotFound)
        ));
    }

    /// The application an action is aimed at is decided by the physical
    /// trigger, not by the click that picks a slot: by then the ring's own
    /// overlay is in front, and on Windows that is where `SendInput` would
    /// land. Activation must therefore hand back the target `begin` was given,
    /// never a freshly read one.
    #[test]
    fn activation_replays_the_trigger_time_target() {
        let manager = ActionRingManager::default();
        let triggered_over = crate::runtime::ActionDispatchTarget::SafariProcess(417);
        let invocation = manager.begin(ActionRingSessionSpec {
            target: triggered_over,
            ..spec()
        });
        let activation = manager
            .activate(invocation.session_id, ActionRingSlot::Top)
            .expect("a live session must activate its top slot");
        assert_eq!(activation.target, triggered_over);
    }

    #[test]
    fn repeated_hover_is_deduplicated() {
        let manager = ActionRingManager::default();
        let invocation = manager.begin(spec());
        assert!(
            manager
                .hover(invocation.session_id, ActionRingSlot::Top)
                .is_ok_and(|hover| hover.is_some())
        );
        assert_eq!(
            manager.hover(invocation.session_id, ActionRingSlot::Top),
            Ok(None)
        );
    }

    #[tokio::test]
    async fn cancellation_stops_the_ring_being_shown() {
        let manager = ActionRingManager::default();
        let invocation = manager.begin(spec());
        manager.cancel(invocation.session_id);
        assert_eq!(manager.observe(0).await.invocation, None);
    }

    #[test]
    fn replacement_invalidates_the_previous_session() {
        let manager = ActionRingManager::default();
        let first = manager.begin(spec());
        let second = manager.begin(spec());
        assert!(matches!(
            manager.activate(first.session_id, ActionRingSlot::Top),
            Err(ActionRingCommandError::SessionNotFound)
        ));
        manager
            .activate(second.session_id, ActionRingSlot::Top)
            .expect("the replacement session must still be activatable");
    }

    /// The two clocks start at different moments — the session when `begin`
    /// stamps it, the window once the long poll has delivered and the overlay
    /// is up — so equal durations expire the session while the ring is still
    /// on screen, and the click that lands there is silently dropped.
    #[test]
    fn a_session_outlives_the_window_it_serves() {
        assert!(
            SESSION_LIFETIME > DISPLAY_LIFETIME,
            "a click on a still-visible ring must still find its session"
        );
    }

    #[test]
    fn expired_session_rejects_interaction() {
        let manager = ActionRingManager::default();
        let invocation = manager.begin(spec());
        let mut state = manager.state();
        let session = state.active.as_mut().expect("begin creates a session");
        session.opened_at = Instant::now()
            .checked_sub(SESSION_LIFETIME + Duration::from_secs(1))
            .expect("test instant has sufficient history");
        drop(state);

        assert!(matches!(
            manager.activate(invocation.session_id, ActionRingSlot::Top),
            Err(ActionRingCommandError::SessionNotFound)
        ));
    }
    #[test]
    fn input_region_is_scoped_validated_and_expires_fail_open() {
        use openlogi_core::action_ring::{INPUT_LEASE, InputRegion};
        let manager = ActionRingManager::default();
        let first = manager.begin(spec());
        let region = InputRegion {
            x: -540.,
            y: 0.,
            width: 540.,
            height: 540.,
        };
        assert!(manager.set_input_region(first.session_id, Some(region)));
        assert_eq!(manager.outside_session(-200., 200.), None);
        assert_eq!(manager.outside_session(1., 200.), Some(first.session_id));
        assert_eq!(
            manager.outside_session_at(1., 200., Instant::now() + INPUT_LEASE),
            None
        );
        let next = manager.begin(spec());
        assert!(!manager.set_input_region(first.session_id, Some(region)));
        assert_eq!(manager.outside_session(1., 200.), None);
        assert!(manager.set_input_region(next.session_id, Some(region)));
        manager.cancel(first.session_id);
        assert_eq!(manager.outside_session(1., 200.), Some(next.session_id));
        assert!(manager.set_input_region(
            next.session_id,
            Some(InputRegion {
                width: f64::NAN,
                ..region
            })
        ));
        assert_eq!(manager.outside_session(1., 200.), None);
        assert!(manager.set_input_region(next.session_id, Some(region)));
        manager.cancel(next.session_id);
        assert_eq!(manager.outside_session(1., 200.), None);
    }
}
