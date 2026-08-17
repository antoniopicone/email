//! The SASL XOAUTH2 mechanism, as used by Gmail.
//!
//! The client sends a single initial response of the form
//!
//! ```text
//! user=<email>^Aauth=Bearer <token>^A^A
//! ```
//!
//! where `^A` is 0x01. The `imap` crate base64-encodes whatever we return, so
//! we hand back the raw string.

pub struct XOAuth2 {
    pub user: String,
    pub access_token: String,
}

impl XOAuth2 {
    pub fn new(user: impl Into<String>, access_token: impl Into<String>) -> Self {
        Self { user: user.into(), access_token: access_token.into() }
    }
}

impl imap::Authenticator for XOAuth2 {
    type Response = String;

    fn process(&self, _challenge: &[u8]) -> Self::Response {
        format!("user={}\x01auth=Bearer {}\x01\x01", self.user, self.access_token)
    }
}
