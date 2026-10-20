mod crypto;
pub mod enrollment;
mod http;
pub mod maintenance;
mod store;
pub use http::router;
pub(crate) use store::Session as DoctorSession;
pub mod views;

pub trait Clock: Send + Sync {
  fn now(&self) -> i64;
  fn now_millis(&self) -> i64 {
    self.now().saturating_mul(1000)
  }
}
pub struct SystemClock;
impl Clock for SystemClock {
  fn now(&self) -> i64 {
    chrono::Utc::now().timestamp()
  }
  fn now_millis(&self) -> i64 {
    chrono::Utc::now().timestamp_millis()
  }
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
  #[error("Doctor access is unavailable")]
  Unavailable,
  #[error("Invalid credentials or enrollment proof")]
  Invalid,
  #[error("Too many attempts")]
  Limited(i64),
  #[error("Doctor is already enrolled; setup cannot replace access")]
  Exists,
}
impl From<sea_orm::DbErr> for Error {
  fn from(_: sea_orm::DbErr) -> Self {
    Self::Unavailable
  }
}

#[cfg(test)]
pub(crate) mod tests;
