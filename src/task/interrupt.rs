use async_trait::async_trait;
use futures::{select, Future, FutureExt};
use stop_token::StopToken;

/// ResolveOrStop allows futures to be resolved, or stopped mid-resolution.
#[async_trait]
pub trait ResolveOrStop {
    type Output;

    /// Returns the future's output if it resolves, or `None` if the StopToken resolves first.
    async fn resolve_or_stop(self, stop_token: &mut StopToken) -> Option<Self::Output>;
}

#[async_trait]
// The Send bound is necessary due to the + Send bound `async_trait` provides on the return type.
impl<F: Future + Send> ResolveOrStop for F {
    type Output = F::Output;

    async fn resolve_or_stop(self, stop_token: &mut StopToken) -> Option<Self::Output> {
        select! {
            res = self.fuse() => Some(res),
            _ = stop_token.fuse() => None
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use stop_token::StopSource;
    use tokio::time;

    #[tokio::test]
    async fn test_resolves_future_if_unstopped() {
        let fut = async { 5 };
        let stop_source = StopSource::new();
        let res = fut.resolve_or_stop(&mut stop_source.token()).await;
        assert!(res.is_some());
    }

    #[tokio::test]
    async fn test_gives_none_if_stopped() {
        // We must artificially limit how long this future takes to resolve,
        // as it is random which future resolves by select! if two are ready.
        let fut = time::sleep(Duration::from_secs(5));
        let stop_source = StopSource::new();
        let mut stop_token = stop_source.token();

        drop(stop_source);
        let res = fut.resolve_or_stop(&mut stop_token).await;
        assert!(res.is_none());
    }
}
