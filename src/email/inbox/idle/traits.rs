//! The traits module holds traits that generalize `async-imap`'s types into traits, and implementations for those types.
//! These implementations very loosely generalize their counterparts.

use async_imap::{error::Result as IMAPResult, extensions::idle::Handle};
use async_trait::async_trait;

use crate::{IMAPSession, IMAPTransportStream};

/// `IntoIdler` represents somethting that can turn into an `[Idler]`
pub trait IntoIdler {
    type OutputIdler: Idler;

    fn begin_idle(self) -> Self::OutputIdler;
}

/// `Idler` is an sbtraction over [`Handle`]. Notably, though, it does not include the `wait` methods that make it a
/// true Idler (making the name of this trait a bit misleading, but it's what it represents...). This was not done
/// because the signature is a bit tricky to represent in traits, for the same reason async traits are hard to
/// represnet; it returns a tuple where the first element is `impl Future` ([which is notoriously hard to work around](
/// https://smallcultfollowing.com/babysteps/blog/2019/10/26/async-fn-in-traits-are-hard/)), and though it could be
/// worked around, it's not worth it.
#[async_trait]
pub trait Idler {
    type DoneIdleable: IntoIdler;

    async fn init(&mut self) -> IMAPResult<()>;
    async fn done(self) -> IMAPResult<Self::DoneIdleable>;
}

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
