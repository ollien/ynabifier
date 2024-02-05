//! The idle module handles some of the intricacies of the IDLE IMAP operation, including state tracking and
//! performing the "wait"s needed. It also includes traits that loosely generalize `async-imap`s idle-related types.

use async_trait::async_trait;
use std::{fmt::Debug, time::Duration};
use thiserror::Error;

use crate::{IMAPSession, IMAPTransportStream};
use async_imap::{
    error::{Error as IMAPError, Result as IMAPResult},
    extensions::idle::{Handle, IdleResponse},
    imap_proto::Response as IMAPResponse,
};

pub use state::SessionCell;

mod state;

// 29 minutes, as per RFC2177 which specifies we should re-issue our idle every 29 minutes
const WAIT_TIMEOUT: Duration = Duration::from_secs(29 * 60);

/// `IntoIdler` represents something that can turn into an `[Idler]`
pub trait IntoIdler {
    type OutputIdler: Idler;

    fn begin_idle(self) -> Self::OutputIdler;
}

/// `Idler` is an abstraction over [`Handle`]. Notably, though, it does not include the `wait` methods that make it a
/// true Idler (making the name of this trait a bit misleading, but it's what it represents...). This was not done
/// because the signature is a bit tricky to represent in traits, for the same reason async traits are hard to
/// represent; it returns a tuple where the first element is `impl Future` ([which is notoriously hard to work around](
/// https://smallcultfollowing.com/babysteps/blog/2019/10/26/async-fn-in-traits-are-hard/)), and though it could be
/// worked around, it's not worth it.
#[async_trait]
pub trait Idler {
    type DoneIdleable: IntoIdler;

    async fn init(&mut self) -> IMAPResult<()>;
    async fn done(self) -> IMAPResult<Self::DoneIdleable>;
}

/// Error is a thin wrapper around [`IMAPError`], with the ability to differentiate between different failure cases.
#[derive(Error, Debug)]
#[allow(clippy::enum_variant_names)]
pub enum Error {
    /// Indicates that an IDLE command timed out, and should be re-issued to continue
    #[error("idle timed out")]
    NeedNewIdle,
    #[error("was disconnected during idle")]
    NeedReconnect,
    #[error("{0}")]
    AsyncIMAPError(IMAPError),
}

impl From<IMAPError> for Error {
    fn from(err: IMAPError) -> Self {
        Self::AsyncIMAPError(err)
    }
}

/// Data is a type-safe wrapper for [`IdleResponse`]. This acts as a wrapper type so
/// we can extract the response data from a response (the data stored in this variant has a private type).
pub struct Data(IdleResponse);

impl IntoIdler for IMAPSession {
    type OutputIdler = Handle<IMAPTransportStream>;
    fn begin_idle(self) -> Self::OutputIdler {
        self.idle()
    }
}

#[async_trait]
impl Idler for Handle<IMAPTransportStream> {
    type DoneIdleable = IMAPSession;

    async fn init(&mut self) -> IMAPResult<()> {
        Handle::<IMAPTransportStream>::init(self).await
    }

    async fn done(self) -> IMAPResult<Self::DoneIdleable> {
        Handle::<IMAPTransportStream>::done(self).await
    }
}

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
        IdleResponse::Timeout => Err(Error::NeedNewIdle),
        IdleResponse::ManualInterrupt => Err(Error::NeedReconnect),
        IdleResponse::NewData(_) => Ok(Data::new(idle_response)),
    }
}
