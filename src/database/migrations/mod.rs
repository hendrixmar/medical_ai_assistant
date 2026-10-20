pub use sea_orm_migration::prelude::*;

mod m202610070001_doctor_access;
mod m202610070002_practice_configuration;
mod m202610140001_whatsapp_inbox;
mod m202610140002_conversation;
mod m202610140003_doctor_number;
mod m202610140004_google_calendar;
mod m202610160001_appointments;
mod m202610160002_receipts;
mod m202610160003_provider_recovery;
mod m202610160004_application_outbox;
mod m202610200001_doctor_number_fingerprint_key;

pub struct Migrator;

#[async_trait::async_trait]
impl MigratorTrait for Migrator {
  fn migrations() -> Vec<Box<dyn MigrationTrait>> {
    // Unknown historical account migrations remain rejected by SeaORM.
    vec![
      Box::new(m202610070001_doctor_access::Migration),
      Box::new(m202610070002_practice_configuration::Migration),
      Box::new(m202610140001_whatsapp_inbox::Migration),
      Box::new(m202610140002_conversation::Migration),
      Box::new(m202610140003_doctor_number::Migration),
      Box::new(m202610140004_google_calendar::Migration),
      Box::new(m202610160001_appointments::Migration),
      Box::new(m202610160002_receipts::Migration),
      Box::new(m202610160003_provider_recovery::Migration),
      Box::new(m202610160004_application_outbox::Migration),
      Box::new(m202610200001_doctor_number_fingerprint_key::Migration),
    ]
  }
}
