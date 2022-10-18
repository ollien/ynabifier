use async_trait::async_trait;
use std::error::Error;
use std::fmt::{Display, Formatter};
pub(crate) use {register::Registry, stop::Resolution as StopResolution, stop::ResolveOrStop};

use futures::Future;
use thiserror::Error;

mod register;
mod stop;

/// `SpawnError` describes why a spawn may have failed to occur.
#[derive(Error, Debug)]
pub struct SpawnError(String);

impl Display for SpawnError {
    fn fmt(&self, f: &mut Formatter) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Cancel represents a task that can be cancelled
pub trait Cancel {
    /// Cancel the given task. The specifics of how this operates are runtime dependent, but typically
    /// it will stop the task at the next `await`.
    fn cancel(self);
}

/// Join will wait for a task to finish, ignoring its return value.
#[async_trait]
pub trait Join {
    type Error: Error;

    async fn join(self) -> Result<(), Self::Error>;
}

pub trait Handle: Cancel + Join {}

/// Spawn allows for runtime-independent spawning of tasks.
///
/// This is very similar to [`futures::task::Spawn`] except that it allows for cancellation, which
/// that trait does not.
pub trait Spawn {
    type Handle: Handle + Send + Sync;

    /// Spawn a task on an executor, returning a function to cancel the task.
    ///
    /// # Errors
    /// If the executor failed to spawn the task, a [`SpawnError`] is returned
    fn spawn<F: Future + Send + 'static>(&self, future: F) -> Result<Self::Handle, SpawnError>
    where
        <F as Future>::Output: Send;
}
