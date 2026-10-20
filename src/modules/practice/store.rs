use super::model::{Details, Practice, Service};
use crate::database::Db;
use sea_orm::{ConnectionTrait, DbBackend, DbErr, FromQueryResult, Statement, Value};

fn sql(query: &str, values: Vec<Value>) -> Statement {
  Statement::from_sql_and_values(DbBackend::Postgres, query, values)
}

pub async fn load(db: &Db) -> Result<Practice, DbErr> {
  let row = db
    .conn
    .query_one(sql(
      "SELECT to_jsonb(practice) AS configuration FROM practice WHERE id = 1",
      vec![],
    ))
    .await?
    .ok_or_else(|| DbErr::Custom("Practice is unavailable".into()))?;
  serde_json::from_value(row.try_get("", "configuration")?)
    .map_err(|_| DbErr::Custom("Invalid Practice configuration".into()))
}

pub async fn services(db: &Db) -> Result<Vec<Service>, DbErr> {
  db.conn
    .query_all(sql(
      "SELECT id, name, duration_minutes, price_centavos FROM services ORDER BY id",
      vec![],
    ))
    .await?
    .iter()
    .map(|row| Service::from_query_result(row, ""))
    .collect()
}

pub async fn service(db: &Db, service: Service) -> Result<(), DbErr> {
  db.conn.execute(sql("INSERT INTO services (id, name, duration_minutes, price_centavos) VALUES ($1, $2, $3, $4)
    ON CONFLICT (id) DO UPDATE SET name = EXCLUDED.name, duration_minutes = EXCLUDED.duration_minutes, price_centavos = EXCLUDED.price_centavos",
    vec![service.id.into(), service.name.into(), service.duration_minutes.into(), service.price_centavos.into()])).await?;
  Ok(())
}

pub async fn schedule(db: &Db, schedule: super::schedule::Schedule) -> Result<(), DbErr> {
  let hours = serde_json::to_value(schedule.weekly_hours)
    .map_err(|_| DbErr::Custom("Invalid weekly hours".into()))?;
  let result = db.conn.execute(sql("UPDATE practice SET weekly_hours = $1, buffer_before_minutes = $2, buffer_after_minutes = $3, minimum_notice_minutes = $4, grid_minutes = $5, horizon_days = $6, cancellation_policy = $7, reminder_2h_enabled = $8 WHERE id = 1", vec![
    hours.into(), schedule.buffer_before_minutes.into(), schedule.buffer_after_minutes.into(), schedule.minimum_notice_minutes.into(), schedule.grid_minutes.into(), schedule.horizon_days.into(), schedule.cancellation_policy.into(), schedule.reminder_2h_enabled.into(),
  ])).await?;
  if result.rows_affected() != 1 {
    return Err(DbErr::Custom("Practice is unavailable".into()));
  }
  Ok(())
}

pub async fn payments(db: &Db, payments: super::payments::Payments) -> Result<(), DbErr> {
  let result = db.conn.execute(sql("UPDATE practice SET bank = $1, beneficiary = $2, clabe = $3, receipt_window_minutes = $4, review_margin_minutes = $5, grace_minutes = $6, review_interval_minutes = $7, urgent_notice_minutes = $8 WHERE id = 1", vec![
    payments.bank.into(), payments.beneficiary.into(), payments.clabe.into(), payments.receipt_window_minutes.into(), payments.review_margin_minutes.into(), payments.grace_minutes.into(), payments.review_interval_minutes.into(), payments.urgent_notice_minutes.into(),
  ])).await?;
  if result.rows_affected() != 1 {
    return Err(DbErr::Custom("Practice is unavailable".into()));
  }
  Ok(())
}

pub async fn details(db: &Db, details: Details) -> Result<(), DbErr> {
  let result = db
    .conn
    .execute(sql(
      "UPDATE practice SET name = $1, location = $2, timezone = $3 WHERE id = 1",
      vec![
        details.name.into(),
        details.location.into(),
        details.timezone.name().into(),
      ],
    ))
    .await?;
  if result.rows_affected() != 1 {
    return Err(DbErr::Custom("Practice is unavailable".into()));
  }
  Ok(())
}
