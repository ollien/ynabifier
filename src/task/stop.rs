use async_trait::async_trait;
use futures::{select, Future, FutureExt};

/// Represents the whether or not a result came from [`ResolveOrStop::resolve_or_stop`] or if the stop future resolved.
#[derive(Debug)]
pub enum Resolution<T> {
    Resolved(T),
    Stopped,
}

/// `ResolveOrStop` allows futures to be resolved, or stopped mid-resolution.
#[async_trait]
#[allow(clippy::module_name_repetitions)]
pub trait ResolveOrStop {
    type Output;

    /// Returns the future's output if it resolves, or `None` if the given stop future resolves first.
    /// This stop future can be something like a `StopToken`.
    async fn resolve_or_stop<S: Future + Send>(self, stop_future: S) -> Resolution<Self::Output>;
}

#[async_trait]
impl<F: Future + Send> ResolveOrStop for F {
    type Output = F::Output;

    async fn resolve_or_stop<S: Future + Send>(self, stop_future: S) -> Resolution<Self::Output> {
        select! {
            res = self.fuse() => Resolution::Resolved(res),
            _ = stop_future.fuse() => Resolution::Stopped
        }
    }
}

impl<T> Resolution<T> {
    /// `was_stopped` indicates whether or not [`ResolveOrStop::resolve_or_stop`] was stopped prematurely due to a stop signal
    #[allow(unused)]
    pub fn was_stopped(&self) -> bool {
        matches!(self, Self::Stopped)
    }

    /// `resolved` indicates whether or not [`ResolveOrStop::resolve_or_stop`] yielded a value.
    #[allow(unused)]
    pub fn was_resolved(&self) -> bool {
        matches!(self, Self::Resolved(_))
    }

    /// Convert this into an Option, with `None` indicating that his was stopped
    pub fn into_option(self) -> Option<T> {
        match self {
            Self::Resolved(v) => Some(v),
            Self::Stopped => None,
        }
    }

    /// Unwraps the given value, if a value exists.
    ///
    /// # Panics
    /// Panics if the this result was stopped.
    pub fn unwrap(self) -> T {
        self.into_option().unwrap()
    }
}

impl<T, E> Resolution<Result<T, E>> {
    pub fn transpose_result(self) -> Result<Resolution<T>, E> {
        match self {
            Self::Resolved(Ok(val)) => Ok(Resolution::Resolved(val)),
            Self::Resolved(Err(err)) => Err(err),
            Self::Stopped => Ok(Resolution::Stopped),
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::needless_pass_by_value)]
    use std::time::Duration;

    use super::*;
    use std::io;
    use stop_token::StopSource;
    use test_case::test_case;
    use tokio::time;

    #[tokio::test]
    async fn test_resolves_future_if_unstopped() {
        let fut = async { 5 };
        let stop_source = StopSource::new();
        let res = fut.resolve_or_stop(&mut stop_source.token()).await;
        assert!(res.was_resolved());
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
        assert!(res.was_stopped());
    }

    #[test_case(Resolution::Stopped, true)]
    #[test_case(Resolution::Resolved(5), false)]
    fn test_was_stopped(resolution: Resolution<i32>, expected: bool) {
        assert_eq!(expected, resolution.was_stopped());
    }

    #[test_case(Resolution::Stopped, false)]
    #[test_case(Resolution::Resolved(5), true)]
    fn test_was_resolved(resolution: Resolution<i32>, expected: bool) {
        assert_eq!(expected, resolution.was_resolved());
    }

    #[test]
    fn test_into_option_stopped() {
        assert!(Resolution::Stopped::<i32>.into_option().is_none());
    }

    #[test]
    fn test_into_option_resolved() {
        let maybe = Resolution::Resolved(5).into_option();
        assert_eq!(5, maybe.unwrap());
    }

    #[test]
    fn test_transpose_ok_value() {
        let transposed = Resolution::Resolved(Ok::<_, io::Error>(5)).transpose_result();
        assert!(matches!(transposed, Ok(Resolution::Resolved(5))));
    }

    #[test]
    fn test_transpose_stopped() {
        let transposed = Resolution::Stopped::<Result<i32, io::Error>>.transpose_result();
        assert!(matches!(transposed, Ok(Resolution::Stopped)));
    }

    #[test]
    fn test_transpose_error() {
        let transposed = Resolution::Resolved(Err::<i32, _>(io::Error::new(
            io::ErrorKind::Other,
            "oh no!",
        )))
        .transpose_result();

        assert_eq!("oh no!", &transposed.unwrap_err().to_string());
    }
}
