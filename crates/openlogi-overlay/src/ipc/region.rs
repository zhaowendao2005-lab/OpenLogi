//! Input grants use their own channel: hover coalescing must not discard registration.
use openlogi_core::action_ring::{INPUT_RENEWAL, InputRegion};
use tokio::sync::watch;

pub(crate) type RegionPublisher = watch::Sender<Option<(u64, InputRegion)>>;
pub(crate) fn channel() -> (RegionPublisher, watch::Receiver<Option<(u64, InputRegion)>>) {
    watch::channel(None)
}

/// Dropping an old window cannot disarm a replacement window's input region.
pub(crate) struct RegionLease {
    sender: RegionPublisher,
    session: u64,
}
impl RegionLease {
    pub(crate) fn new(sender: RegionPublisher, session: u64, region: InputRegion) -> Self {
        sender.send_replace(Some((session, region)));
        Self { sender, session }
    }
}
impl Drop for RegionLease {
    fn drop(&mut self) {
        self.sender.send_if_modified(|value| {
            if value.is_some_and(|(session, _)| session == self.session) {
                *value = None;
                true
            } else {
                false
            }
        });
    }
}

pub(super) async fn run(mut regions: watch::Receiver<Option<(u64, InputRegion)>>) {
    let mut client = None;
    let mut registered = None;
    loop {
        let region = *regions.borrow_and_update();
        let request = region
            .map(|(session, region)| (session, Some(region)))
            .or_else(|| registered.map(|session| (session, None)));
        if let Some((session, region)) = request {
            if client.is_none() {
                client = super::connect().await;
            }
            if let Some(active) = &client {
                // A timed-out RPC may still have armed the agent: remember it for removal.
                registered = Some(session);
                match tokio::time::timeout(
                    INPUT_RENEWAL,
                    active.action_ring_input_region(tarpc::context::current(), session, region),
                )
                .await
                {
                    Ok(Ok(true)) => registered = region.map(|_| session),
                    Ok(Ok(false)) => registered = None,
                    _ => {
                        client = None;
                    }
                }
            }
        }
        tokio::select! {
            changed = regions.changed() => { if changed.is_err() { break; } }
            () = tokio::time::sleep(INPUT_RENEWAL), if region.is_some() || registered.is_some() => {}
        }
    }
    // A failed final removal is safe: the agent expires the short lease.
    if let (Some(active), Some(session)) = (client, registered) {
        let _ = tokio::time::timeout(
            INPUT_RENEWAL,
            active.action_ring_input_region(tarpc::context::current(), session, None),
        )
        .await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn old_window_drop_does_not_remove_new_region() {
        let (sender, receiver) = channel();
        let region = InputRegion {
            x: 0.,
            y: 0.,
            width: 360.,
            height: 360.,
        };
        let old = RegionLease::new(sender.clone(), 1, region);
        let new = RegionLease::new(sender, 2, region);
        drop(old);
        assert_eq!(receiver.borrow().map(|v| v.0), Some(2));
        drop(new);
        assert!(receiver.borrow().is_none());
    }
}
