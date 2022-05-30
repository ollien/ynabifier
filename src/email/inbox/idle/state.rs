//! The state module implements structures that can abstractly handle the state needed for the IDLE IMAP operation.

use super::traits::{Idler, IntoIdler};
use async_imap::error::Result as IMAPResult;

pub struct IdlerCell<I> {
    idler: I,
    prepared: bool,
}

/// `IdlerCell` is a thin wrapper around an `Idler` to ensure that `init` is only sent as needed.
impl<I: Idler> IdlerCell<I> {
    fn new(idler: I) -> Self {
        Self {
            idler,
            prepared: false,
        }
    }

    /// `prepare` initializes an Idler only if it hasn't already been initialized.
    pub async fn prepare(&mut self) -> IMAPResult<&mut I> {
        if self.prepared {
            return Ok(&mut self.idler);
        }

        self.idler.init().await?;
        self.prepared = true;

        Ok(&mut self.idler)
    }

    pub fn into_inner(self) -> I {
        self.idler
    }
}

/// Represents the state of a possibly iddling session, which may be able to produce an [`Idler`] or
/// have already produced one.
#[allow(clippy::module_name_repetitions)] // I think this is less clear called `Session`.
pub enum SessionState<S, I> {
    Initialized(S),
    IdleReady(IdlerCell<I>),
}

impl<S: IntoIdler<OutputIdler = I>, I: Idler<DoneIdleable = S>> SessionState<S, I> {
    fn new(into_idler: S) -> Self {
        Self::Initialized(into_idler)
    }
}

/// Holds a `SessionState` and allows progression between its various states.
pub struct SessionCell<S, I> {
    // semantically, this will never be `None` between method calls. It is required as an implementation detail
    // of `get_idle_handle`
    state: Option<SessionState<S, I>>,
}

impl<S: IntoIdler<OutputIdler = I>, I: Idler<DoneIdleable = S>> SessionCell<S, I> {
    pub fn new(into_idler: S) -> Self {
        Self {
            state: Some(SessionState::new(into_idler)),
        }
    }

    /// Starts a new idle session if possible, or will return itself if the session is already started.
    pub fn get_idler_cell(&mut self) -> &mut IdlerCell<I> {
        // In normal operation, this can't happen. This can only happen if the following assignment panics
        // after the call to `take()`
        assert!(self.state.is_some(), "invariant violated: state is None");

        self.state = self.state.take().map(|state| match state {
            // If we've already gotten ready to idle, then we can just pass it right through
            SessionState::IdleReady(idle_cell) => SessionState::IdleReady(idle_cell),
            // ...otherwise, we have to begin the idle
            SessionState::Initialized(session) => {
                let idle_cell = IdlerCell::new(session.begin_idle());
                SessionState::IdleReady(idle_cell)
            }
        });

        match &mut self.state {
            Some(SessionState::IdleReady(idler)) => idler,
            _ => panic!("unreachable"),
        }
    }

    /// Dissolve the cell into its inner state
    pub fn into_state(self) -> SessionState<S, I> {
        self.state.expect("invariant violated: state is None")
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use async_imap::error::Result as IMAPResult;
    use async_trait::async_trait;
    use futures::lock::Mutex;

    #[derive(Default)]
    struct MockSession;
    #[derive(Default)]
    struct MockIdler {
        // This has to be a Mutex for a couple reasons
        // 1) some implementations of the Ilder trait use pointer wrappers, so we can't mutate directly
        // 2) it's async, so we can't use RefCell (they're not Send)
        num_init_calls: Mutex<u32>,
    }

    macro_rules! impl_mocks {
        (
            $session: ty: $make_session: expr,
            $idler: ty: $make_idler: expr$(,)?
        ) => {
            impl IntoIdler for $session {
                type OutputIdler = $idler;

                fn begin_idle(self) -> Self::OutputIdler {
                    $make_idler
                }
            }

            #[async_trait]
            impl Idler for $idler {
                type DoneIdleable = $session;

                async fn init(&mut self) -> IMAPResult<()> {
                    *self.num_init_calls.lock().await += 1;
                    Ok(())
                }

                async fn done(self) -> IMAPResult<Self::DoneIdleable> {
                    Ok($make_session)
                }
            }
        };
    }

    #[rustfmt::skip]
    impl_mocks!(
        MockSession: MockSession::default(),
        MockIdler: MockIdler::default(),
    );
    // So we can assert same-ness, we implement a MockSession that can produce a MockIdler
    impl_mocks!(
        Arc<MockSession>: Arc::new(MockSession::default()),
        Arc<MockIdler>: Arc::new(MockIdler::default())
    );

    #[test]
    fn test_unwraps_into_initialized_if_idler_not_received() {
        let session_cell = SessionCell::new(MockSession);
        assert!(matches!(
            session_cell.into_state(),
            SessionState::Initialized(_),
        ));
    }

    #[test]
    fn test_unwraps_into_idler_after_getting_idler_once() {
        let mut session_cell = SessionCell::new(MockSession);
        session_cell.get_idler_cell();

        assert!(matches!(
            session_cell.into_state(),
            SessionState::IdleReady(_),
        ));
    }

    #[tokio::test]
    async fn test_gets_same_idler_every_time() {
        let mut session_cell = SessionCell::new(Arc::new(MockSession));

        let idle_cell1 = session_cell
            .get_idler_cell()
            .prepare()
            .await
            .expect("prepare failed")
            // We want to get the Arc<Idler> back, but we can't borrow mutably twice, so we clone it.
            .clone();

        let idle_cell2 = session_cell
            .get_idler_cell()
            .prepare()
            .await
            .expect("prepare failed");

        assert!(Arc::ptr_eq(&idle_cell1, idle_cell2));
    }

    #[tokio::test]
    async fn test_gets_same_idler_after_unwrap() {
        let mut session_cell = SessionCell::new(Arc::new(MockSession));

        let idler1 = session_cell
            .get_idler_cell()
            .prepare()
            .await
            .expect("prepare failed")
            // We want to get the Arc<Idler> back, but we can't borrow mutably twice, so we clone it.
            .clone();

        if let SessionState::IdleReady(mut idler_cell) = session_cell.into_state() {
            let idler2 = idler_cell.prepare().await.expect("failed to prepare");
            assert!(Arc::ptr_eq(&idler1, idler2));
        } else {
            panic!("didn't get idle ready when expected");
        }
    }

    #[tokio::test]
    async fn test_repeatedly_calling_prepare_does_not_call_init_more_than_once() {
        let mut session_cell = SessionCell::new(MockSession);
        let idler_cell = session_cell.get_idler_cell();
        let mock_idler = idler_cell.prepare().await.expect("prepare failed");

        assert_eq!(1, *mock_idler.num_init_calls.lock().await);

        // we know from `test_gets_same_idler_every_time` that these are returning the same idler under the hood.

        let mock_idler = idler_cell.prepare().await.expect("prepare failed");
        assert_eq!(1, *mock_idler.num_init_calls.lock().await);
    }

    #[tokio::test]
    async fn test_idle_cell_into_inner() {
        let mut idler_cell = IdlerCell::new(Arc::new(MockIdler::default()));
        let idler = idler_cell
            .prepare()
            .await
            .expect("failed to prepare")
            .clone();

        let inner_idler = idler_cell.into_inner();
        assert!(Arc::ptr_eq(&idler, &inner_idler));
    }
}
