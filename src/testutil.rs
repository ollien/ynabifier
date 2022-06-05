//! Provides items that may be useful for testing

use futures::Future;

use crate::task::{Cancel, Spawn, SpawnError};

#[derive(Clone)]
pub struct TokioSpawner;
impl Spawn for TokioSpawner {
    type Cancel = CancelFnOnce;

    fn spawn<F: Future + Send + 'static>(&self, future: F) -> Result<Self::Cancel, SpawnError>
    where
        <F as Future>::Output: Send,
    {
        let handle = tokio::spawn(future);
        let canceler = CancelFnOnce {
            cancel_func: Box::new(move || handle.abort()),
        };

        Ok(canceler)
    }
}

pub struct CancelFnOnce {
    cancel_func: Box<dyn FnOnce() + Send + Sync>,
}

impl Cancel for CancelFnOnce {
    fn cancel(self) {
        (self.cancel_func)();
    }

    fn cancel_boxed(self: Box<Self>) {
        self.cancel();
    }
}
