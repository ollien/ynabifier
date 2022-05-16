//! The idle module handles some of the intricacies of the IDLE IMAP operation, including state tracking and
//! performing the "wait"s needed

use std::{fmt::Debug, time::Duration};
use thiserror::Error;

use async_imap::{
    error::Error as IMAPError,
    extensions::idle::{Handle, IdleResponse},
    imap_proto::Response as IMAPResponse,
};

use crate::{IMAPSession, IMAPTransportStream};

pub use state::IdlerCell;

mod state;
mod traits;

// 29 minutes, as per RFC2177 which specifies we should re-issue our idle every 29 minutes
const WAIT_TIMEOUT: Duration = Duration::from_secs(29 * 60);

/// Error is a thin wrapper around [`IMAPError`], with the ability to differentiate between different failure cases.
#[derive(Error, Debug)]
pub enum Error {
    /// Indicates that an IDLE command timed out, and should be re-issued to continue
    #[error("idle timed out")]
    Timeout,
    #[error("{0}")]
    AsyncIMAPError(IMAPError),
}

impl From<IMAPError> for Error {
    fn from(err: IMAPError) -> Self {
        Self::AsyncIMAPError(err)
    }
}

/// `SessionState` holds the state of an idling session, and whether or not we have access to the current session or
/// the idle handle.
pub enum SessionState {
    Initialized(IMAPSession),
    IdleReady(IdlerCell<Handle<IMAPTransportStream>>),
}

/// Holds a `SessionState` and allows progression between its various states.
pub struct SessionCell {
    inner: state::SessionCell<IMAPSession, Handle<IMAPTransportStream>>,
}

impl SessionCell {
    /// Make a new `SessionCell` from the given session. The initial state of this cell is [`SessionState::Initialized`]
    pub fn new(session: IMAPSession) -> Self {
        Self {
            inner: state::SessionCell::new(session),
        }
    }

    /// Get the idle handle, if one already exists, or produce a new one from the current Session.
    pub fn get_idler_cell(&mut self) -> &mut IdlerCell<Handle<IMAPTransportStream>> {
        self.inner.get_idler_cell()
    }

    /// Dissolve this cell, and get the state that session was in.
    pub fn into_state(self) -> SessionState {
        match self.inner.into_state() {
            state::SessionState::Initialized(session) => SessionState::Initialized(session),
            state::SessionState::IdleReady(handle) => SessionState::IdleReady(handle),
        }
    }
}

/// Data is a type-safe wrapper for [`IdleResponse`]. This acts as a wrapper type so
/// we can extract the response data from a response (the data stored in this variant has a private type).
pub struct Data(IdleResponse);

impl Data {
    /// `new` constructs a new `IdleData` from an [`IdleResponse`] containing data.
    ///
    /// # Panics
    /// Will panic if the response does not have the variant of `IdleResponse::newData`. This is a private module,
    /// where we should control the data going in, so we really do consider this unrecoverable.
    pub fn new(response: IdleResponse) -> Self {
        assert!(matches!(response, IdleResponse::NewData(_)));

        Self(response)
    }

    /// `response` gets the server's response out of our `Data`.
    ///
    /// # Panics
    /// This can panic if `Data`'s type storage invariant is violated.
    pub fn response(&self) -> &IMAPResponse {
        match &self.0 {
            IdleResponse::NewData(data) => data.parsed(),
            _ => panic!("not possible by construction"),
        }
    }
}

pub async fn wait_for_data(idle_handle: &mut Handle<IMAPTransportStream>) -> Result<Data, Error> {
    let (idle_response_future, _stop) = idle_handle.wait_with_timeout(WAIT_TIMEOUT);
    let idle_response = idle_response_future.await?;
    match idle_response {
        IdleResponse::ManualInterrupt => panic!("we don't interrupt manually"),
        IdleResponse::Timeout => Err(Error::Timeout),
        IdleResponse::NewData(_) => Ok(Data::new(idle_response)),
    }
}
