//! Provides items that may be useful for testing
#![cfg(test)]

use async_trait::async_trait;
use futures::Future;
use tokio::task::{JoinError, JoinHandle};

use crate::task::{Cancel, Handle, Join, Spawn, SpawnError};

#[derive(Clone)]
pub struct TokioSpawner;
impl Spawn for TokioSpawner {
    type Handle = JoinHandle<()>;

    fn spawn<F: Future + Send + 'static>(&self, future: F) -> Result<Self::Handle, SpawnError>
    where
        <F as Future>::Output: Send,
    {
        let handle = tokio::spawn(async move {
            future.await;
        });

        Ok(handle)
    }
}

impl<T> Cancel for JoinHandle<T> {
    fn cancel(self) {
        self.abort();
    }
}

#[async_trait]
impl<T: Send> Join for JoinHandle<T> {
    type Error = JoinError;

    async fn join(self) -> Result<(), Self::Error> {
        match (self as JoinHandle<T>).await {
            Err(err) => Err(err),
            Ok(_) => Ok(()),
        }
    }
}

impl<T: Send> Handle for JoinHandle<T> {}
