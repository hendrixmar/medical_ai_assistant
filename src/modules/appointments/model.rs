use sea_orm::FromQueryResult;
use uuid::Uuid;

#[derive(Clone, FromQueryResult)]
pub struct Appointment {
  pub id: Uuid,
  pub contact_id: Uuid,
  pub patient_name: String,
  pub service_id: Uuid,
  pub service_name: String,
  pub duration_minutes: i32,
  pub price_centavos: i64,
  pub format: String,
  pub starts_at: i64,
  pub ends_at: i64,
  pub timezone: String,
  pub location: String,
  pub buffer_before_minutes: i32,
  pub buffer_after_minutes: i32,
  pub status: String,
  pub payment: String,
  pub calendar_sync: String,
  pub bank: Option<String>,
  pub beneficiary: Option<String>,
  pub clabe: Option<String>,
  pub payment_reference: Option<String>,
  pub receipt_deadline: Option<i64>,
  pub booking_calendar: Option<String>,
  pub google_generation: Option<i64>,
  pub google_selection_version: Option<i64>,
  pub event_id: Option<String>,
  pub event_etag: Option<String>,
  pub meet_url: Option<String>,
  pub revision: i64,
  pub attendance_confirmed_at: Option<i64>,
  pub cancelled_at: Option<i64>,
  pub expired_at: Option<i64>,
  pub created_at: i64,
  pub updated_at: i64,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Format {
  InPerson,
  Video,
}

impl Format {
  pub fn as_str(self) -> &'static str {
    match self {
      Self::InPerson => "in_person",
      Self::Video => "video",
    }
  }

  pub fn from_db(value: &str) -> Option<Self> {
    match value {
      "in_person" => Some(Self::InPerson),
      "video" => Some(Self::Video),
      _ => None,
    }
  }
}
