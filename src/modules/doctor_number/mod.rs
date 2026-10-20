mod crypto;
mod http;
pub(crate) mod store;
mod views;
pub(crate) use http::router;

#[derive(Debug, thiserror::Error)]
pub enum Error {
  #[error("Doctor Number unavailable")]
  Unavailable,
  #[error("Challenge issuance limited")]
  Limited(i64),
}
impl From<sea_orm::DbErr> for Error {
  fn from(_: sea_orm::DbErr) -> Self {
    Self::Unavailable
  }
}

#[cfg(test)]
mod tests;
