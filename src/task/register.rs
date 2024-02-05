//! Provides the [`Registry`] type, which can be used to manage a set of tasks

use std::collections::HashMap;

use std::mem;
use uuid::Uuid;

use super::Handle;

/// A `TaskToken` is a unique identifier that represents a task in Registry.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct TaskToken(Uuid);

// A set of tasks that can be stopped as a whole. Each task can be registered individually, and
// unregistered at any point.
pub struct Registry<H> {
    handles: HashMap<TaskToken, H>,
}

impl<H: Handle> Registry<H> {
    pub fn new() -> Self {
        Self {
            handles: HashMap::new(),
        }
    }

    /// Register a task handle, and get back a unique [`TaskToken`] to handle its lifetime.
    pub fn register_handle(&mut self, handle: H) -> TaskToken {
        let token = TaskToken(Uuid::new_v4());
        self.handles.insert(token, handle);
        token
    }

    /// Unregister a handle using its task token. Once a task has been unregistered, [`Registry::cancel_all`] and
    /// [`Registry::join_all`] will have no effect on this task.
    pub fn unregister_handle(&mut self, token: TaskToken) {
        self.handles.remove(&token);
    }

    /// Cancel all of the the tasks in this registry. If either [`Registry::cancel_all`] or [`Registry::join_all`] are
    /// called after this, the tasks that were currently in the registry will no longer be acted
    /// on. In effect, these tasks are unregistered before cancellation.
    // yes this is technically unused but I don't really care for the warning right now
    #[allow(dead_code)]
    pub fn cancel_all(&mut self) {
        let handles = mem::take(&mut self.handles);
        for handle in handles.into_values() {
            handle.cancel();
        }
    }

    /// Join all of the the tasks in this registry. If either [`Registry::cancel_all`] or [`Registry::join_all`] are
    /// called after this, the tasks that were currently in the registry will no longer be acted
    /// on. In effect, these tasks are unregistered before joining.
    ///
    /// # Errors
    ///
    /// A list of all the join errors that occurred will be returned.
    pub async fn join_all(&mut self) -> Result<(), Vec<H::Error>> {
        let handles = mem::take(&mut self.handles);

        let mut errors = Vec::new();
        for handle in handles.into_values() {
            if let Err(err) = handle.join().await {
                errors.push(err);
            }
        }

        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{ops::AddAssign, sync::Arc, time::Duration};

    use super::*;
    use futures::{channel::oneshot, lock::Mutex};
    use tokio::time;

    use crate::{task::Spawn, testutil::TokioSpawner};

    fn unpack_pairs<T, U, I: Iterator<Item = (T, U)>>(iter: I) -> (Vec<T>, Vec<U>) {
        iter.fold((vec![], vec![]), |mut res, (tx, rx)| {
            res.0.push(tx);
            res.1.push(rx);
            res
        })
    }

    #[tokio::test]
    async fn cancel_call_stops_all_tasks() {
        let (txs_to_task, rxs_in_task) = unpack_pairs((1..=5).map(|_| oneshot::channel::<()>()));
        let (txs_in_task, rxs_from_task) = unpack_pairs((1..=5).map(|_| oneshot::channel::<()>()));
        let spawner = TokioSpawner;
        let handles = rxs_in_task
            .into_iter()
            .zip(txs_in_task)
            .map(|(rx_in_task, tx_in_task)| async move {
                rx_in_task.await.expect("failed to rx in task");
                tx_in_task.send(()).expect("failed to tx in task");
            })
            .map(|task| spawner.spawn(task).expect("failed to spawn task"));

        let mut registry = Registry::new();
        for handle in handles {
            registry.register_handle(handle);
        }

        registry.cancel_all();

        // Unblock all the tasks
        for tx in txs_to_task {
            // it's ok if this fails - the task might have already been cancelled
            let _res = tx.send(());
        }

        for rx in rxs_from_task {
            time::timeout(Duration::from_secs(5), async move {
                rx.await.expect_err("should not have received from tasks");
            })
            .await
            .expect("test timed out");
        }
    }

    #[tokio::test]
    async fn dropping_registry_should_not_cancel_tasks() {
        let (txs_to_task, rxs_in_task) = unpack_pairs((1..=5).map(|_| oneshot::channel::<()>()));
        let (txs_in_task, rxs_from_task) = unpack_pairs((1..=5).map(|_| oneshot::channel::<()>()));
        let spawner = TokioSpawner;
        let handles = rxs_in_task
            .into_iter()
            .zip(txs_in_task)
            .map(|(rx_in_task, tx_in_task)| async move {
                rx_in_task.await.expect("failed to rx in task");
                tx_in_task.send(()).expect("failed to tx in task");
            })
            .map(|task| spawner.spawn(task).expect("failed to spawn task"));

        let mut registry = Registry::new();
        for handle in handles {
            registry.register_handle(handle);
        }

        // Unblock all the tasks
        for tx in txs_to_task {
            tx.send(()).expect("failed to tx into task");
        }

        drop(registry);
        for rx in rxs_from_task {
            time::timeout(Duration::from_secs(5), async move {
                rx.await.expect("should not received from tasks");
            })
            .await
            .expect("test timed out");
        }
    }

    #[tokio::test]
    async fn join_all_waits_for_task_completion() {
        let count = Arc::new(Mutex::new(0));
        let (txs_to_task, rxs_in_task) = unpack_pairs((1..=500).map(|_| oneshot::channel::<()>()));
        let spawner = TokioSpawner;
        let handles = rxs_in_task
            .into_iter()
            .map(|rx_in_task| {
                let count_clone = count.clone();
                async move {
                    rx_in_task.await.expect("failed to rx in task");
                    // This is a little ham-fisted, but gives us some assurance that the tasks won't finish
                    // before we're ready for them.
                    time::sleep(Duration::from_millis(100)).await;
                    count_clone.lock().await.add_assign(1);
                }
            })
            .map(|task| spawner.spawn(task).expect("failed to spawn task"));

        let mut registry = Registry::new();
        for handle in handles {
            registry.register_handle(handle);
        }

        // Unblock all the tasks
        for tx in txs_to_task {
            tx.send(()).expect("failed to tx into task");
        }

        let join_res = time::timeout(Duration::from_secs(5), registry.join_all())
            .await
            .expect("test timed out");

        join_res.expect("failed to join tasks");

        assert_eq!(500, *count.lock().await);
    }

    #[tokio::test]
    async fn removing_token_prevents_it_from_being_joined() {
        let count = Arc::new(Mutex::new(0));
        let (txs_to_task, rxs_in_task) = unpack_pairs((1..=500).map(|_| oneshot::channel::<()>()));
        let spawner = TokioSpawner;
        let handles = rxs_in_task
            .into_iter()
            .map(|rx_in_task| {
                let count_clone = count.clone();
                async move {
                    rx_in_task.await.expect("failed to rx in task");
                    // This is a little ham-fisted, but gives us some assurance that the tasks won't finish
                    // before we're ready for them.
                    time::sleep(Duration::from_millis(100)).await;
                    count_clone.lock().await.add_assign(1);
                }
            })
            .map(|task| spawner.spawn(task).expect("failed to spawn task"));

        let mut registry = Registry::new();
        for handle in handles {
            registry.register_handle(handle);
        }

        let (_tx, never_rx) = oneshot::channel::<()>();
        let infinite_task_handle = spawner.spawn(never_rx).expect("failed to spawn");
        let infinite_task_token = registry.register_handle(infinite_task_handle);

        // Unblock all the finite tasks
        for tx in txs_to_task {
            tx.send(()).expect("failed to tx into task");
        }

        registry.unregister_handle(infinite_task_token);

        let join_res = time::timeout(Duration::from_secs(5), registry.join_all())
            .await
            .expect("test timed out; it's possible the task wasn't unregistered");

        join_res.expect("failed to join tasks");
    }

    #[tokio::test]
    async fn removing_token_prevents_it_from_being_cancelled() {
        let (txs_to_task, rxs_in_task) = unpack_pairs((1..=5).map(|_| oneshot::channel::<()>()));
        let (txs_in_task, rxs_from_task) = unpack_pairs((1..=5).map(|_| oneshot::channel::<()>()));
        let spawner = TokioSpawner;
        let mut handles = rxs_in_task
            .into_iter()
            .zip(txs_in_task)
            .map(|(rx_in_task, tx_in_task)| async move {
                rx_in_task.await.expect("failed to rx in task");
                tx_in_task.send(()).expect("failed to tx in task");
            })
            .map(|task| spawner.spawn(task).expect("failed to spawn task"));

        let mut registry = Registry::new();
        let first_task_token = registry.register_handle(handles.next().unwrap());
        for handle in handles {
            registry.register_handle(handle);
        }

        registry.unregister_handle(first_task_token);
        registry.cancel_all();

        // Unblock all the tasks
        for tx in txs_to_task {
            // it's ok if this fails - the task might have already been cancelled
            let _res = tx.send(());
        }

        let mut rx_iter = rxs_from_task.into_iter();
        // Ensure we receive from the task we unregistered
        rx_iter
            .next()
            .unwrap()
            .await
            .expect("should have received from unregistered task");

        // ...but not from any of the others
        for rx in rx_iter {
            time::timeout(Duration::from_secs(5), async move {
                rx.await.expect_err("should not have received from tasks");
            })
            .await
            .expect("test timed out");
        }
    }
}
