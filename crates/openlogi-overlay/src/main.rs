//! Lightweight GPUI host for the cursor-centred Actions Ring.
//!
//! This process is a pure IPC client. The agent owns HID++, session validation,
//! haptic output, and action execution; the overlay only renders the
//! agent-snapshotted actions and reports hover/activate/cancel interactions.

#![cfg_attr(
    all(target_os = "windows", not(debug_assertions)),
    windows_subsystem = "windows"
)]

// `t!` resolves against a backend the invoking crate must generate itself, so
// both binaries expand `i18n!` over the one catalog in `openlogi-ui` — the same
// crate this one already depends on for locale negotiation.
rust_i18n::i18n!("../openlogi-ui/locales", fallback = "en");

mod ipc;
mod platform;
mod ring;
mod session;

use std::sync::Arc;

use anyhow::Result;
use gpui::AppContext as _;
use tracing::warn;

use openlogi_core::action_ring::DISPLAY_LIFETIME;

use crate::ipc::OverlayCommand;
use crate::platform::RingPlacement;
use crate::ring::RingView;
use crate::session::{ClickAwaySession, claim_the_role, spawn_click_away_dismissal};

fn main() -> Result<()> {
    openlogi_core::logging::init_stderr();

    openlogi_core::locale::activate(None);
    // Held for the whole run: dropping it hands the role to the replacement.
    let _tenancy = claim_the_role()?;
    let ipc::Handle {
        regions,
        mut invocations,
        commands,
    } = ipc::spawn();

    let mut app = gpui_platform::application().with_assets(openlogi_ui::action_icons::ActionIcons);
    app = app.with_quit_mode(gpui::QuitMode::Explicit);
    app.run(move |cx| {
        platform::configure_application();
        let live_session = Arc::new(ClickAwaySession::new());
        spawn_click_away_dismissal(cx, Arc::clone(&live_session));
        cx.spawn(async move |cx| {
            while let Some(observed) = invocations.recv().await {
                // No ring is what a dismissal looks like: close whatever is
                // showing and open nothing. The agent has already forgotten the
                // session, so there is nothing to acknowledge either.
                let Some(invocation) = observed else {
                    cx.update(|cx| {
                        for handle in cx.windows() {
                            if let Some(handle) = handle.downcast::<RingView>() {
                                let _ = handle.update(cx, RingView::dismiss);
                            }
                        }
                    });
                    continue;
                };
                openlogi_core::locale::activate(invocation.language.as_deref());
                let session_id = invocation.session_id;
                let started = std::time::Instant::now();
                let created = cx.update(|cx| -> Result<_> {
                    for handle in cx.windows() {
                        let _ =
                            handle.update(cx, |_, window, cx| platform::close_window(window, cx));
                    }
                    let placement = RingPlacement::capture(cx)?;
                    let handle = cx.open_window(placement.window_options(), |_, cx| {
                        cx.new(|_| {
                            RingView::new(invocation, commands.clone(), &live_session)
                                .with_region_publisher(regions.clone())
                        })
                    })?;
                    Ok((placement, handle))
                });
                let (placement, handle) = match created {
                    Ok(created) => created,
                    Err(error) => {
                        warn!(%error, "could not open Actions Ring window");
                        let _ = commands.send(OverlayCommand::Cancel { session_id });
                        continue;
                    }
                };
                // Native positioning must run after App/Window's mutable borrows
                // have ended: Win32 synchronously reenters their event callbacks.
                if let Err(error) = placement.show(handle, cx) {
                    warn!(%error, "could not position Actions Ring window");
                    let _ = handle.update(cx, |_, window, cx| platform::close_window(window, cx));
                    let _ = commands.send(OverlayCommand::Cancel { session_id });
                    continue;
                }
                tracing::debug!(
                    session_id,
                    elapsed_ms = started.elapsed().as_millis(),
                    "Actions Ring window ready"
                );
                cx.spawn(async move |cx| {
                    cx.background_executor().timer(DISPLAY_LIFETIME).await;
                    let _ = handle.update(cx, RingView::cancel);
                })
                .detach();
            }
        })
        .detach();
    });
    Ok(())
}

#[cfg(test)]
mod tests;
