pub mod actions;
pub mod http;
pub mod intake;
mod views;

#[cfg(test)]
mod tests;
pub use intake::{receive, ReceiveOutcome, Unavailable};
