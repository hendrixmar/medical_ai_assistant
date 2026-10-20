mod http;
pub(crate) mod model;
pub(crate) mod payments;
pub(crate) mod schedule;
pub(crate) mod store;
mod views;
pub(crate) use http::router;
pub(crate) use schedule::{next_working_time, Day as PracticeDay};

#[cfg(test)]
mod tests;
