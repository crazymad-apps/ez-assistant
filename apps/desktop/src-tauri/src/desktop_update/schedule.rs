use std::future::Future;
use tokio_util::sync::CancellationToken;

pub(super) async fn poll<F: Future<Output = ()>>(
    stop: &CancellationToken,
    mut check: impl FnMut() -> F,
) {
    let mut interval = tokio::time::interval(super::CHECK_INTERVAL);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            biased;
            () = stop.cancelled() => break,
            _ = interval.tick() => {
                tokio::select! {
                    biased;
                    () = stop.cancelled() => break,
                    () = check() => {},
                }
            }
        }
    }
}
