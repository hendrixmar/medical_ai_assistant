pub mod client;
pub mod config;
mod http;
pub mod store;
mod views;
pub mod worker;
pub use http::router;

#[cfg(test)]
mod tests;
#[cfg(test)]
mod worker_tests;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum Error {
  #[error("Invalid Mercado Pago request")]
  Invalid,
  #[error("Mercado Pago rejected the request")]
  Rejected,
  #[error("Mercado Pago unavailable")]
  Unavailable,
}
