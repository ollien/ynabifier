//! The `multi` module provides utilities to run multiple tasks together

use std::{fmt::Debug, sync::Arc};

use thiserror::Error;
use super::{Spawn, SpawnError, Cancel};
use futures::{channel::{mpsc::{self, UnboundedSender, UnboundedReceiver}, oneshot}, Future, StreamExt, SinkExt};
pub type Task = Box<dyn Future<Output = ()> + Send>;

#[derive(Error, Debug)]
pub enum TaskError {
    #[error("The task accepting loop is stopped")]
   ReceiverGone
}

/// Runner is returned by [`listen_for_tasks`]. See its docstring for more details.
pub struct Runner {
    task_sender: UnboundedSender<Task>,
}

pub struct CancelHandle {
    task_sender: UnboundedSender<Task>,
    cancel_signal_sender: oneshot::Sender<()>
}

/// Spawn a background task to listen for new tasks and run them to completion. The given `Runner` can be cancelled
/// as a unit, which will cancel all submitted tasks
///
/// # Errors
/// Returns a [`SpawnError`] if creating the necessary background tasks failed.
pub fn listen_for_tasks<S: Spawn + Send + Sync + 'static>(spawner: Arc<S>) -> Result<(Runner, CancelHandle), SpawnError> {
    let (task_sender, task_receiver) = mpsc::unbounded();
    let (cancel_sender, cancel_receiver) = mpsc::unbounded();
    let (cancel_signal_sender, cancel_signal_receiver) = oneshot::channel();

    let cancel_cancel_loop = spawner.spawn(cancel_on_signal(cancel_signal_receiver, cancel_receiver))?;
    let spawner_clone = spawner.clone();
    let spawn_res = spawner_clone.spawn(run_tasks_on_recv(spawner, task_receiver, cancel_sender));
    if let Err(err) = spawn_res {
        // We shouldn't leave the cancel task running endlessly if we failed to spawn
        cancel_cancel_loop.cancel();
        return Err(err);
    }

    let cancel_handle = CancelHandle{task_sender: task_sender.clone(), cancel_signal_sender};
    let runner = Runner{task_sender};
    Ok((runner, cancel_handle))
}

impl Runner {
    /// Submit a task to the associated task loop.
    ///
    /// # Errors
    /// Returns a [`TaskError`] if there was a failure in submitting the task to the runner task
    #[allow(clippy::missing_panics_doc)]
    pub async fn submit_new_task(&mut self, task: Task) -> Result<(), TaskError> {
        self.task_sender.send(task).await.map_err(|err| {
            // Because we have an unbounded sender, the only possibility of failure is disconnection
            assert!(err.is_disconnected());
            assert!(!err.is_full());
            TaskError::ReceiverGone
        })
    }
}

impl Cancel for CancelHandle {
    fn cancel(self) {
        // We don't care about if this Send fails; it will only
        // fail if the other end has hung up, which means that
        // we've done our cancellation job anyway
        let _ = self.cancel_signal_sender.send(());
        self.task_sender.close_channel();
    }

    fn cancel_boxed(self: Box<Self>) {
        self.cancel();
    }
}


async fn run_tasks_on_recv<S> (
    spawner: Arc<S>,
    mut task_stream: UnboundedReceiver<Task>,
    mut cancel_sink: UnboundedSender<Box<dyn Cancel + Send>>
) where
    S: Spawn + Send + Sync,
    S::Cancel: 'static,
{
    while let Some(task) = task_stream.next().await {
        let spawn_res = spawner.spawn(Box::into_pin(task));
        match spawn_res {
            Ok(cancel) => {
                let send_cancel_res = cancel_sink.send(Box::new(cancel)).await;
                if let Err(err) = send_cancel_res {
                    warn!(
                        concat!(
                        "a task successfully spawned, but its Cancel could not be stored. ",
                        "This may lead to a leak! : {:?}"
                    ), err);
                }
            }
            // TODO: Maybe we could embed some metadata so we know what task failed to run
            Err(err) => error!("failed to spawn task: {}", err)
        }
    }
}

/// Wait for a cancellation signal on the given oneshot channel, and cancel all Cancels
/// in the `cancel_stream` upon receipt.
async fn cancel_on_signal(
    cancel_signal_receiver: oneshot::Receiver<()>,
    mut cancel_stream: UnboundedReceiver<Box<dyn Cancel + Send>>
) {
    // If signaler has hung up, that's fine, we are just gonna die out and ignore cancellation.
    if cancel_signal_receiver.await.is_ok() {
        while let Some(cancel) = cancel_stream.next().await {
            cancel.cancel_boxed();
        }
    }
}


#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TokioSpawner;
    use std::{
        task::{Context, Poll},
        time::Duration,
        pin::Pin
    };
    use futures::{
        FutureExt,
        future,
        channel::oneshot
    };
    use tokio::time;

    fn unpack_pairs<T, U, I: Iterator<Item = (T, U)>>(iter: I) -> (Vec<T>, Vec<U>) {
        iter.fold((vec![], vec![]), |mut res, (tx, rx)| {
            res.0.push(tx);
            res.1.push(rx);
            res
        })
    }

    #[tokio::test]
    async fn test_submitted_tasks_are_scheduled_to_run() {
        let (txs_from_task, rxs_from_task) = unpack_pairs((1..=5).map(|_| oneshot::channel::<()>()));
        let (mut runner, _) = listen_for_tasks(Arc::new(TokioSpawner))
            .expect("failed to begin listening for tasks");

        // Each task will send a message once they've been called
        let tasks = txs_from_task.into_iter().map(|tx| async { tx.send(()).expect("failed to send result"); });
        for task in tasks {
            let submit_res = runner.submit_new_task(Box::new(task)).await;
            assert!(submit_res.is_ok());
        }


        // Wait for all messages to come back (i.e. all the tasks have been run)
        let all_rcvd = future::join_all(rxs_from_task.into_iter());
        time::timeout(Duration::from_secs(5), all_rcvd).await
            .expect("task timed out")
            .into_iter().for_each(|res| res.expect("receive failed"));
    }

    struct DropHandle {
       _tx: oneshot::Sender<()>
    }

    struct DropWaiter {
        rx: oneshot::Receiver<()>
    }

    impl Future for DropWaiter {
        type Output = ();
        fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
            match self.rx.poll_unpin(cx) {
                Poll::Pending => Poll::Pending,
                Poll::Ready(Err(_)) => {
                    Poll::Ready(())
                }
                Poll::Ready(res) => panic!("got a canceled result: {:?}", res)
            }
        }
    }

    fn drop_channel() -> (DropHandle, DropWaiter) {
        let (tx, rx) = oneshot::channel();
        (
            DropHandle{_tx: tx},
            DropWaiter{rx},
        )
    }

    #[tokio::test]
    async fn test_canceling_the_runner_cancels_task() {
        let (txs_to_task, rxs_from_task) = unpack_pairs((1..=5).map(|_| oneshot::channel::<()>()));
        let (drop_handles, drop_waiters) = unpack_pairs((1..=5).map(|_| drop_channel()));

        let (mut runner, cancel_handle) = listen_for_tasks(Arc::new(TokioSpawner))
            .expect("failed to begin listening for tasks");
        let tasks = rxs_from_task
            .into_iter()
            .zip(drop_handles.into_iter())
            .map(|(rx_from_task, drop_handle)| {
            async move {
                // We wait for this to drop so we can wait on this asynchronous cancellation
                #[allow(clippy::no_effect_underscore_binding)]
                let _drop_handle = drop_handle;
                rx_from_task.await.expect("recv failed");
                panic!("this should never be reached");
            }
        });

        for task in tasks {
            let res = runner.submit_new_task(Box::new(task)).await;
            assert!(res.is_ok());
        }

        cancel_handle.cancel();
        let all_dropped = future::join_all(drop_waiters);
        time::timeout(Duration::from_secs(5), all_dropped).await
            .expect("waiting for drop timed out");

        // Once we know the handle has been dropped, we can make sure the task is dead
        // by trying to send something into it and making sure we fail to do so.
        txs_to_task
            .into_iter()
            .for_each(|tx| tx.send(()).expect_err("The receiver is still alive unexpectedly"));
    }

    #[tokio::test]
    async fn test_does_not_cancel_on_drop() {
        let (seed_tx, seed_rx) = oneshot::channel();
        let (response_tx, response_rx) = oneshot::channel();
        let (mut runner, _cancel_handle) = listen_for_tasks(Arc::new(TokioSpawner))
            .expect("failed to begin listening for tasks");

        let task = async {
            let value = seed_rx.await
                .expect("failed to receive value in task");
            response_tx.send(value)
                .expect("failed to echo value from task");
        };

        runner.submit_new_task(Box::new(task)).await
            .expect("failed to submit task");

        drop(runner);
        seed_tx.send(100)
            .expect("could not send message into task");
        let rcvd_value = response_rx.await
            .expect("failed to receive from task");
        // If the task is still running, even after dropping the runner, we should get this value back
        assert!(rcvd_value == 100);
    }

    #[tokio::test]
    async fn test_cannot_submit_new_tasks_after_drop() {
        let (mut runner, cancel_handle) = listen_for_tasks(Arc::new(TokioSpawner))
            .expect("failed to begin listening for tasks");

        cancel_handle.cancel();
        runner.submit_new_task(Box::new(async {}))
            .await
            .expect_err("should not have been able to submit a task");
    }
}
