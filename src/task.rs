pub use cancel::{Cancel, Multi as MultiCancel, OnDrop as CancelOnDrop};
use std::fmt::{Display, Formatter};

use futures::Future;
use thiserror::Error;

mod cancel;
pub mod multi;

/// `SpawnError` describes why a spawn may have failed to occur.
#[derive(Error, Debug)]
pub struct SpawnError(String);

impl Display for SpawnError {
    fn fmt(&self, f: &mut Formatter) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Spawn allows for runtime-independent spawning of tasks.
///
/// This is very similar to [`futures::task::Spawn`] except that it allows for cancellation, which
/// that trait does not.
pub trait Spawn {
    type Cancel: Cancel + Send + Sync;

    /// Spawn a task on an executor, returning a function to cancel the task.
    ///
    /// # Errors
    /// If the executor failed to spawn the task, a [`SpawnError`] is returned
    fn spawn<F: Future + Send + 'static>(&self, future: F) -> Result<Self::Cancel, SpawnError>
    where
        <F as Future>::Output: Send;
}
