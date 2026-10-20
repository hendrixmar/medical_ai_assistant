pub mod appointments;
mod calendars;
pub mod client;
pub mod config;
mod credentials;
mod crypto;
mod http;
pub mod maintenance;
mod probes;
mod store;
pub mod views;
pub mod worker;
pub use http::router;

#[derive(Debug, thiserror::Error)]
pub enum Error {
  #[error("Google Calendar unavailable")]
  Unavailable,
  #[error("Google Calendar request invalid or stale")]
  Invalid,
  #[error("Google Calendar action limited")]
  Limited,
  #[error("Google Calendar requires authorization")]
  Reauthorize,
}
impl From<sea_orm::DbErr> for Error {
  fn from(_: sea_orm::DbErr) -> Self {
    Self::Unavailable
  }
}
#[cfg(test)]
pub(crate) mod tests;
