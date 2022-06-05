//! Provides utilities for canceling tasks

/// Cancel represents a task that can be cancelled
pub trait Cancel {
    /// Cancel the given task. The specifics of how this operates are runtime dependent, but typically
    /// it will stop the task at the next `await`.
    fn cancel(self);

    /// Identical to [`cancel`] but allows operation on a Boxed Self. Typicaly, this can just call `cancel`(),
    /// but a default implementation cannot be provided, lest we bound this to [`Sized`]s.
    fn cancel_boxed(self: Box<Self>);
}

/// "Aggregates" cancels, and allows cancellation for all of them at once. This is useful
/// when many subtasks are spawned, and one may want to cancel all of the tasks associated with it.
#[derive(Default)]
pub struct Multi {
    cancels: Vec<Box<dyn Cancel>>,
}

impl Multi {
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert a new [`Cancel`] into the Multi
    pub fn insert(&mut self, cancel: Box<dyn Cancel>) {
        self.cancels.push(cancel);
    }

    // Cancel all of the `cancel`s in the `Multi`.
    pub fn cancel_all(&mut self) {
        while let Some(cancel) = self.cancels.pop() {
            cancel.cancel_boxed();
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{cell::RefCell, rc::Rc};

    use super::*;

    struct CancelCounter {
        count: Rc<RefCell<u16>>,
    }

    impl Cancel for CancelCounter {
        fn cancel(self) {
            *self.count.borrow_mut() += 1;
        }

        fn cancel_boxed(self: Box<Self>) {
            self.cancel();
        }
    }

    #[test]
    fn test_cancels_all() {
        let mut multi = Multi::new();

        let cancel_count = Rc::new(RefCell::new(0_u16));
        let cancels = vec![
            CancelCounter {
                count: cancel_count.clone(),
            },
            CancelCounter {
                count: cancel_count.clone(),
            },
            CancelCounter {
                count: cancel_count.clone(),
            },
        ];

        for cancel in cancels {
            multi.insert(Box::new(cancel));
        }

        multi.cancel_all();
        assert_eq!(3, *cancel_count.borrow());
    }
}
