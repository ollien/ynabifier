use std::{fmt::Debug, time::Duration};

use async_imap::{
    extensions::idle::{Handle, IdleResponse},
    imap_proto::{MailboxDatum, Response},
    Session,
};
use futures::{AsyncRead, AsyncWrite, StreamExt};

const WAIT_TIMEOUT: Duration = Duration::from_secs(29 * 60);

mod idle {
    use super::{IdleResponse, Response};

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
        /// This can panic if `Data`'s type storage invariant is violted.
        pub fn response(&self) -> &Response {
            match &self.0 {
                IdleResponse::NewData(data) => data.parsed(),
                _ => panic!("not possible by construction"),
            }
        }
    }
}

/// `fetch_email` will consume the current session and put it into an `Idle` state, until a new email is received
///
/// # Errors
/// If, for any reason, the email fails to be fetched, one of `async_imap` error's will be returned.
#[allow(clippy::module_name_repetitions)]
pub async fn fetch_email<T>(session: Session<T>) -> async_imap::error::Result<Session<T>>
where
    T: AsyncRead + AsyncWrite + Unpin + Debug + Send,
{
    // TODO: check if the server can handle IDLE
    let mut idle_handle = session.idle();
    idle_handle.init().await?;
    let response_data = idle_until_data_received(&mut idle_handle).await?;
    let response = response_data.response();
    let sequence_number = match response {
        Response::MailboxData(MailboxDatum::Exists(seq)) => seq,
        _ => panic!("no idea what to do with this {:?}", response),
    };

    let mut unidled_session = idle_handle.done().await?;

    // TODO: This should be done somewhere other than this function, in some kind of concurrent task.
    let message = unidled_session
        .fetch(format!("{:?}", sequence_number), "RFC822")
        .await?
        // god please don't do this just to get a single message
        .next()
        .await
        .unwrap()?;

    let parsed = mailparse::parse_mail(message.body().unwrap()).unwrap();
    dbg!(parsed.get_body().unwrap());
    let subparts = parsed.subparts;
    let res = subparts
        .into_iter()
        .map(|x| x.get_body())
        .collect::<Result<String, _>>();
    println!("{}", res.unwrap());

    Ok(unidled_session)
}

async fn idle_until_data_received<T>(
    idle_handle: &mut Handle<T>,
) -> async_imap::error::Result<idle::Data>
where
    T: AsyncRead + AsyncWrite + Unpin + Debug + Send,
{
    loop {
        let (idle_response_future, _stop) = idle_handle.wait_with_timeout(WAIT_TIMEOUT);
        let idle_response = idle_response_future.await?;
        match idle_response {
            IdleResponse::ManualInterrupt => panic!("we don't interrupt manually"),
            IdleResponse::Timeout => continue,
            IdleResponse::NewData(_) => return Ok(idle::Data::new(idle_response)),
        }
    }
}
