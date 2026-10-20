pub mod appointments;
pub mod conversation;
pub mod doctor;
pub(crate) mod doctor_number;
pub mod google_calendar;
pub mod health;
pub mod payments;
pub(crate) mod practice;
pub mod whatsapp;

use axum::Router;

use crate::app::AppState;

pub fn router() -> Router<AppState> {
  Router::new().nest("/api", health::router())
}
