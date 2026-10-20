use crate::{
  common::config::Config,
  database::Db,
  modules::{
    conversation::store::{enqueue_text, Message},
    doctor::Clock,
    google_calendar::client::Client,
    practice::{self, model::Service},
    whatsapp::{bound_transaction, inbox::sql},
  },
};
use chrono::Timelike;
use sea_orm::{ConnectionTrait, DatabaseTransaction, DbErr, FromQueryResult, TransactionTrait};
use uuid::Uuid;

use super::{
  availability::{self, CalendarState, Choice, Offers, Request, ServiceSnapshot, Target},
  model::{Appointment, Format},
  store,
};

const APPOINTMENT_SELECT: &str = "SELECT id,contact_id,patient_name,service_id,service_name,duration_minutes,price_centavos,format,starts_at,ends_at,timezone,location,buffer_before_minutes,buffer_after_minutes,status,payment,calendar_sync,bank,beneficiary,clabe,payment_reference,receipt_deadline,booking_calendar,google_generation,google_selection_version,event_id,event_etag,meet_url,revision,attendance_confirmed_at,cancelled_at,expired_at,created_at,updated_at FROM appointments";

pub struct SelectionRequest {
  pub contact: Uuid,
  pub service_id: Uuid,
  pub format: Format,
  pub starts_at: i64,
  pub appointment_id: Option<Uuid>,
  pub expected_revision: Option<i64>,
  pub late_recovery: bool,
}

pub enum Prepared {
  Ready(Preflight),
  Refresh(Offers),
  Followup,
  Invalid,
}

pub struct Preflight {
  request: SelectionRequest,
  choice: Choice,
  calendar: CalendarState,
  target: Target,
  practice_snapshot: String,
  service_snapshot: Option<ServiceSnapshot>,
}

pub enum Commit {
  Applied {
    appointment_id: Uuid,
    status: String,
    revision: i64,
  },
  Stale,
  OpenLimit,
  Followup,
}

pub async fn prepare_selection(
  db: &Db,
  clock: &dyn Clock,
  cfg: &Config,
  client: Option<&Client>,
  request: SelectionRequest,
  patient_name: Option<&str>,
) -> Result<Prepared, DbErr> {
  let now = clock.now_millis();
  let appointment = if let Some(id) = request.appointment_id {
    let Some(appointment) = store::get(&db.conn, id).await? else {
      return Ok(Prepared::Invalid);
    };
    if appointment.contact_id != request.contact
      || request.expected_revision != Some(appointment.revision)
      || appointment.service_id != request.service_id
      || Format::from_db(&appointment.format) != Some(request.format)
    {
      return Ok(Prepared::Invalid);
    }
    if request.late_recovery {
      if appointment.status != "expired"
        || appointment.payment != "verified"
        || appointment.format != "video"
        || request.format != Format::Video
      {
        return Ok(Prepared::Invalid);
      }
    } else if !matches!(
      appointment.status.as_str(),
      "requested" | "awaiting_payment" | "confirmed"
    ) || appointment.starts_at <= now
    {
      return Ok(Prepared::Invalid);
    }
    Some(appointment)
  } else {
    if request.late_recovery || request.expected_revision.is_some() {
      return Ok(Prepared::Invalid);
    }
    None
  };
  if appointment.is_none() && !valid_patient(patient_name) {
    return Ok(Prepared::Invalid);
  }
  let active_expiry = if let Some(appointment) = appointment.as_ref() {
    let row = db
      .conn
      .query_one(sql(
        "SELECT expires_at FROM appointment_holds WHERE appointment_id=$1 AND active ORDER BY id LIMIT 1",
        vec![appointment.id.into()],
      ))
      .await?;
    if request.late_recovery {
      if row.is_some() {
        return Ok(Prepared::Invalid);
      }
      None
    } else {
      let Some(row) = row else {
        return Ok(Prepared::Invalid);
      };
      let expires_at: Option<i64> = row.try_get("", "expires_at")?;
      if expires_at.is_some_and(|expires| expires <= now) {
        return Ok(Prepared::Followup);
      }
      expires_at
    }
  } else {
    None
  };

  let practice = practice::store::load(db).await?;
  let fields = practice.fields();
  let Ok(schedule) = practice::schedule::Schedule::parse(&fields) else {
    return Ok(Prepared::Invalid);
  };
  if !request.late_recovery
    && appointment.as_ref().is_some_and(|appointment| {
      appointment.starts_at
        < now.saturating_add(i64::from(schedule.minimum_notice_minutes).saturating_mul(60_000))
    })
  {
    return Ok(Prepared::Followup);
  }
  let Some(timezone) = practice
    .timezone
    .as_deref()
    .and_then(|value| value.parse::<chrono_tz::Tz>().ok())
  else {
    return Ok(Prepared::Invalid);
  };
  let Some(local) = chrono::DateTime::<chrono::Utc>::from_timestamp_millis(request.starts_at)
    .map(|time| time.with_timezone(&timezone))
  else {
    return Ok(Prepared::Invalid);
  };
  if local.second() != 0 || local.nanosecond() != 0 {
    return Ok(Prepared::Invalid);
  }
  let minute = (local.hour() * 60 + local.minute()) as u16;
  let service_id = appointment
    .as_ref()
    .map_or(request.service_id, |appointment| appointment.service_id);
  let saved_payment = appointment
    .as_ref()
    .map(|appointment| appointment.payment.as_str());
  let request_for_availability = Request {
    contact: request.contact,
    service_id,
    format: request.format,
    date: local.date_naive(),
    period: Some((minute, minute.saturating_add(1))),
    exclude_appointment: request.appointment_id,
    duration_override: appointment
      .as_ref()
      .map(|appointment| appointment.duration_minutes),
    payment_state: saved_payment,
    receipt_deadline: appointment
      .as_ref()
      .and_then(|appointment| appointment.receipt_deadline),
    hold_expires_at: active_expiry,
  };
  let offers = availability::offer(db, clock, cfg, client, request_for_availability).await?;
  let Some(choice) = offers
    .choices
    .iter()
    .copied()
    .find(|choice| choice.starts_at == request.starts_at)
  else {
    let mut refresh_request = request_for_availability;
    refresh_request.period = None;
    let refresh = availability::offer(db, clock, cfg, client, refresh_request).await?;
    return Ok(Prepared::Refresh(refresh));
  };
  if offers.calendar == CalendarState::NotConfigured
    || offers.calendar == CalendarState::VideoDisabled
    || request.format == Format::Video && offers.calendar != CalendarState::Checked
  {
    return Ok(Prepared::Invalid);
  }
  let (Some(target), Some(practice_snapshot)) = (offers.target, offers.practice_snapshot) else {
    return Ok(Prepared::Invalid);
  };
  let service_snapshot = offers.service_snapshot;
  if appointment.is_none() && service_snapshot.is_none() {
    return Ok(Prepared::Invalid);
  }
  Ok(Prepared::Ready(Preflight {
    request,
    choice,
    calendar: offers.calendar,
    target,
    practice_snapshot,
    service_snapshot,
  }))
}

pub(crate) async fn open_limit_reached<C: ConnectionTrait>(
  conn: &C,
  contact: Uuid,
  exclude: Option<Uuid>,
  now: i64,
) -> Result<bool, DbErr> {
  let row = conn.query_one(sql(
    "SELECT COUNT(*)>=3 AS reached FROM appointments WHERE contact_id=$1 AND status IN ('requested','awaiting_payment','confirmed') AND starts_at>$2 AND ($3::uuid IS NULL OR id<>$3)",
    vec![contact.into(), now.into(), exclude.into()],
  )).await?.ok_or_else(|| DbErr::Custom("Open Appointment count unavailable".into()))?;
  row.try_get("", "reached")
}

/// Commits the state change, Hold, History, jobs, message association and rendered outbox in one savepoint.
pub async fn commit_selection(
  tx: &DatabaseTransaction,
  mut preflight: Preflight,
  inbox: &str,
  patient_name: Option<&str>,
  origin: &str,
  now: i64,
) -> Result<Commit, DbErr> {
  let work = tx.begin().await?;
  bound_transaction(&work).await?;
  let request = &preflight.request;
  work
    .query_one(sql(
      "SELECT id FROM contacts WHERE id=$1 FOR UPDATE",
      vec![request.contact.into()],
    ))
    .await?
    .ok_or_else(|| DbErr::Custom("Appointment Contact unavailable".into()))?;

  let current = if let Some(id) = request.appointment_id {
    let Some(row) = work
      .query_one(sql(
        &format!("{APPOINTMENT_SELECT} WHERE id=$1 FOR UPDATE"),
        vec![id.into()],
      ))
      .await?
    else {
      work.rollback().await?;
      return Ok(Commit::Stale);
    };
    let current = Appointment::from_query_result(&row, "")?;
    if current.contact_id != request.contact
      || current.revision != request.expected_revision.unwrap_or_default()
      || current.service_id != request.service_id
      || Format::from_db(&current.format) != Some(request.format)
      || !eligible_reschedule(&current, preflight.request.late_recovery, now)
    {
      work.rollback().await?;
      return Ok(Commit::Stale);
    }
    Some(current)
  } else {
    None
  };

  let practice_row = work
    .query_one(sql(
      "SELECT to_jsonb(p) AS configuration FROM practice p WHERE id=1 FOR SHARE",
      vec![],
    ))
    .await?
    .ok_or_else(|| DbErr::Custom("Practice unavailable".into()))?;
  let practice_json: serde_json::Value = practice_row.try_get("", "configuration")?;
  let practice: practice::model::Practice = serde_json::from_value(practice_json)
    .map_err(|_| DbErr::Custom("Invalid Practice configuration".into()))?;
  let fields = practice.fields();
  if serde_json::to_string(&fields).ok().as_deref() != Some(preflight.practice_snapshot.as_str()) {
    work.rollback().await?;
    return Ok(Commit::Stale);
  }
  let details = practice::model::Details::parse(&fields)
    .map_err(|_| DbErr::Custom("Practice details are incomplete".into()))?;
  let schedule = practice::schedule::Schedule::parse(&fields)
    .map_err(|_| DbErr::Custom("Practice schedule is incomplete".into()))?;
  let earliest =
    now.saturating_add(i64::from(schedule.minimum_notice_minutes).saturating_mul(60_000));
  if preflight.choice.starts_at < earliest {
    work.rollback().await?;
    return Ok(Commit::Stale);
  }
  if !request.late_recovery
    && current
      .as_ref()
      .is_some_and(|appointment| appointment.starts_at < earliest)
  {
    work.rollback().await?;
    return Ok(Commit::Followup);
  }
  if current.is_none() && request.format == Format::Video {
    let payments = practice::payments::Payments::parse(&fields)
      .map_err(|_| DbErr::Custom("Video payment settings are incomplete".into()))?;
    let deadline = now
      .saturating_add(i64::from(payments.receipt_window_minutes).saturating_mul(60_000))
      .min(
        preflight
          .choice
          .starts_at
          .saturating_sub(i64::from(payments.review_margin_minutes).saturating_mul(60_000)),
      );
    if deadline.saturating_sub(now) < 30 * 60_000 {
      work.rollback().await?;
      return Ok(Commit::Stale);
    }
    preflight.choice.receipt_deadline = Some(deadline);
    preflight.choice.hold_expires_at =
      Some(deadline.saturating_add(i64::from(payments.grace_minutes).saturating_mul(60_000)));
  }

  if let Some(snapshot) = preflight.service_snapshot.as_ref() {
    let Some(row) = work
      .query_one(sql(
        "SELECT id,name,duration_minutes,price_centavos FROM services WHERE id=$1 FOR SHARE",
        vec![snapshot.id.into()],
      ))
      .await?
    else {
      work.rollback().await?;
      return Ok(Commit::Stale);
    };
    let service = Service::from_query_result(&row, "")?;
    if service.name != snapshot.name
      || service.duration_minutes != snapshot.duration_minutes
      || service.price_centavos != snapshot.price_centavos
    {
      work.rollback().await?;
      return Ok(Commit::Stale);
    }
  }

  if !target_matches(&work, &preflight.target, request.format == Format::Video).await? {
    work.rollback().await?;
    return Ok(Commit::Stale);
  }
  let current = if let Some(current) = current {
    current
  } else {
    if open_limit_reached(&work, request.contact, None, now).await? {
      work.rollback().await?;
      return Ok(Commit::OpenLimit);
    }
    let None = request.appointment_id else {
      work.rollback().await?;
      return Ok(Commit::Stale);
    };
    if !valid_patient(patient_name) {
      work.rollback().await?;
      return Ok(Commit::Stale);
    }
    let service = preflight
      .service_snapshot
      .as_ref()
      .ok_or_else(|| DbErr::Custom("Service snapshot unavailable".into()))?;
    let id = Uuid::now_v7();
    let payment = if request.format == Format::Video {
      "awaiting_receipt"
    } else {
      "not_required"
    };
    let status = match request.format {
      Format::Video => "awaiting_payment",
      Format::InPerson if preflight.calendar == CalendarState::Unavailable => "requested",
      Format::InPerson => "confirmed",
    };
    let calendar_sync = if request.format == Format::Video {
      "not_yet"
    } else {
      "pending"
    };
    let event_id = if request.format == Format::InPerson {
      Some(id.simple().to_string())
    } else {
      None
    };
    let (bank, beneficiary, clabe, reference) = if request.format == Format::Video {
      let payments = practice::payments::Payments::parse(&fields)
        .map_err(|_| DbErr::Custom("Video payment settings are incomplete".into()))?;
      (
        Some(payments.bank),
        Some(payments.beneficiary),
        Some(payments.clabe),
        Some(new_payment_reference(&work).await?),
      )
    } else {
      (None, None, None, None)
    };
    let revision = 1_i64;
    work.execute(sql(
      "INSERT INTO appointments(id,contact_id,patient_name,service_id,service_name,duration_minutes,price_centavos,format,starts_at,ends_at,timezone,location,buffer_before_minutes,buffer_after_minutes,status,payment,calendar_sync,bank,beneficiary,clabe,payment_reference,receipt_deadline,booking_calendar,google_generation,google_selection_version,event_id,revision,created_at,updated_at) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,$18,$19,$20,$21,$22,$23,$24,$25,$26,$27,$28,$28)",
      vec![
        id.into(), request.contact.into(), patient_name.unwrap_or_default().trim().into(), service.id.into(), service.name.clone().into(), service.duration_minutes.into(), service.price_centavos.into(), request.format.as_str().into(), preflight.choice.starts_at.into(), preflight.choice.ends_at.into(), details.timezone.name().into(), details.location.into(), schedule.buffer_before_minutes.into(), schedule.buffer_after_minutes.into(), status.into(), payment.into(), calendar_sync.into(), bank.into(), beneficiary.into(), clabe.into(), reference.clone().into(), preflight.choice.receipt_deadline.into(), preflight.target.calendar.clone().into(), preflight.target.generation.into(), preflight.target.selection_version.into(), event_id.clone().into(), revision.into(), now.into(),
      ],
    )).await?;
    let hold_id = Uuid::now_v7();
    let hold_start = preflight
      .choice
      .starts_at
      .saturating_sub(i64::from(schedule.buffer_before_minutes).saturating_mul(60_000));
    let hold_end = preflight
      .choice
      .ends_at
      .saturating_add(i64::from(schedule.buffer_after_minutes).saturating_mul(60_000));
    if let Err(error) = work.execute(sql(
      "INSERT INTO appointment_holds(id,appointment_id,starts_at,ends_at,active,expires_at) VALUES ($1,$2,$3,$4,true,$5)",
      vec![hold_id.into(), id.into(), hold_start.into(), hold_end.into(), preflight.choice.hold_expires_at.into()],
    )).await {
      work.rollback().await?;
      if error.to_string().contains("appointment_holds_no_cross_appointment_overlap") {
        return Ok(Commit::Stale);
      }
      return Err(error);
    }
    store::history(
      &work,
      id,
      "Contact",
      if status == "requested" {
        "appointment_requested"
      } else if request.format == Format::Video {
        "video_payment_requested"
      } else {
        "appointment_created"
      },
      now,
    )
    .await?;
    store::job(
      &work,
      id,
      "conflict",
      now,
      &format!("{id}:{revision}:conflict"),
    )
    .await?;
    if request.format == Format::Video {
      if let Some(expires) = preflight.choice.hold_expires_at {
        store::job(
          &work,
          id,
          "expiry",
          expires,
          &format!("{id}:{revision}:expiry:deadline"),
        )
        .await?;
      }
    } else {
      store::job(
        &work,
        id,
        "calendar_sync",
        now,
        &format!("{id}:{revision}:calendar_sync"),
      )
      .await?;
    }
    work.execute(sql(
      "INSERT INTO message_appointments(inbox_message_id,appointment_id) VALUES ($1,$2) ON CONFLICT DO NOTHING",
      vec![inbox.into(), id.into()],
    )).await?;
    set_conversation_idle(&work, request.contact, now).await?;
    if request.format == Format::Video {
      enqueue_payment_instructions(
        &work,
        PaymentInstructions {
          appointment: id,
          revision,
          contact: request.contact,
          deadline: preflight
            .choice
            .receipt_deadline
            .ok_or_else(|| DbErr::Custom("Receipt deadline unavailable".into()))?,
          price_centavos: service.price_centavos,
          bank: practice
            .bank
            .as_deref()
            .map(str::trim)
            .ok_or_else(|| DbErr::Custom("Bank snapshot unavailable".into()))?,
          beneficiary: practice
            .beneficiary
            .as_deref()
            .map(str::trim)
            .ok_or_else(|| DbErr::Custom("Beneficiary snapshot unavailable".into()))?,
          clabe: practice
            .clabe
            .as_deref()
            .map(str::trim)
            .ok_or_else(|| DbErr::Custom("CLABE snapshot unavailable".into()))?,
          reference: reference
            .as_deref()
            .ok_or_else(|| DbErr::Custom("Payment reference unavailable".into()))?,
          timezone: details.timezone.name(),
        },
        origin,
        now,
      )
      .await?;
    } else {
      let local = store::local_time(preflight.choice.starts_at, details.timezone.name());
      let text = if status == "requested" {
        format!("Registré su solicitud para el {local}, pero todavía no está confirmada ni puedo prometer disponibilidad. Le avisaré cuando compruebe Google Calendar.")
      } else {
        format!("La confirmación de su cita del {local} está pendiente de Google Calendar. Le avisaré cuando se complete.")
      };
      enqueue_text(
        &work,
        Message {
          contact: request.contact,
          appointment: Some(id),
          key: &format!("appointment:{}:{}:{}", id, revision, "created"),
          text: &text,
          purpose: "status",
          revision: Some(revision),
        },
        origin,
        now,
      )
      .await?;
    }
    work.commit().await?;
    return Ok(Commit::Applied {
      appointment_id: id,
      status: status.into(),
      revision,
    });
  };

  let old_hold = work
      .query_one(sql(
        "SELECT id,expires_at FROM appointment_holds WHERE appointment_id=$1 AND active ORDER BY id LIMIT 1 FOR UPDATE",
        vec![current.id.into()],
      ))
      .await?;
  if !preflight.request.late_recovery && old_hold.is_none() {
    work.rollback().await?;
    return Ok(Commit::Stale);
  }
  if !preflight.request.late_recovery {
    let expiry: Option<i64> = old_hold
      .as_ref()
      .map(|row| row.try_get::<Option<i64>>("", "expires_at"))
      .transpose()?
      .flatten();
    if expiry.is_some_and(|expiry| expiry <= now) {
      work.rollback().await?;
      return Ok(Commit::Stale);
    }
  } else if old_hold.is_some() {
    work.rollback().await?;
    return Ok(Commit::Stale);
  }
  if open_limit_reached(&work, request.contact, Some(current.id), now).await? {
    work.rollback().await?;
    return Ok(Commit::OpenLimit);
  }
  let before = i64::from(schedule.buffer_before_minutes).saturating_mul(60_000);
  let after = i64::from(schedule.buffer_after_minutes).saturating_mul(60_000);
  let hold_start = preflight.choice.starts_at.saturating_sub(before);
  let hold_end = preflight.choice.ends_at.saturating_add(after);
  if other_active_hold(&work, current.id, hold_start, hold_end).await? {
    work.rollback().await?;
    return Ok(Commit::Stale);
  }
  let revision = current
    .revision
    .checked_add(1)
    .ok_or_else(|| DbErr::Custom("Appointment revision exhausted".into()))?;
  let recovering = preflight.request.late_recovery;
  let status = if recovering {
    "confirmed"
  } else {
    current.status.as_str()
  };
  let event_id = if recovering && current.event_id.is_none() {
    Some(current.id.simple().to_string())
  } else {
    current.event_id.clone()
  };
  let sync = if event_id.is_some() {
    "pending"
  } else {
    current.calendar_sync.as_str()
  };
  let new_hold_id = Uuid::now_v7();
  if let Err(error) = work.execute(sql(
    "INSERT INTO appointment_holds(id,appointment_id,starts_at,ends_at,active,expires_at) VALUES ($1,$2,$3,$4,true,$5)",
    vec![new_hold_id.into(), current.id.into(), hold_start.into(), hold_end.into(), preflight.choice.hold_expires_at.into()],
  )).await {
    work.rollback().await?;
    if error.to_string().contains("appointment_holds_no_cross_appointment_overlap") {
      return Ok(Commit::Stale);
    }
    return Err(error);
  }
  work
    .execute(sql(
      "UPDATE appointment_holds SET active=false WHERE appointment_id=$1 AND active AND id<>$2",
      vec![current.id.into(), new_hold_id.into()],
    ))
    .await?;
  work.execute(sql(
    "UPDATE appointments SET starts_at=$2,ends_at=$3,timezone=$4,buffer_before_minutes=$5,buffer_after_minutes=$6,status=$7,calendar_sync=$8,event_id=$9,revision=$10,updated_at=$11 WHERE id=$1",
    vec![current.id.into(), preflight.choice.starts_at.into(), preflight.choice.ends_at.into(), details.timezone.name().into(), schedule.buffer_before_minutes.into(), schedule.buffer_after_minutes.into(), status.into(), sync.into(), event_id.clone().into(), revision.into(), now.into()],
  )).await?;
  store::history(
    &work,
    current.id,
    "Contact",
    if recovering {
      "late_receipt_recovery_rescheduled"
    } else {
      "rescheduled"
    },
    now,
  )
  .await?;
  store::job(
    &work,
    current.id,
    "conflict",
    now,
    &format!("{}:{revision}:conflict", current.id),
  )
  .await?;
  if event_id.is_some() {
    store::job(
      &work,
      current.id,
      "calendar_sync",
      now,
      &format!("{}:{revision}:calendar_sync", current.id),
    )
    .await?;
  }
  if current.status == "awaiting_payment" {
    crate::modules::payments::intake::schedule_pending_payment_jobs(
      &work,
      current.id,
      preflight.choice.starts_at,
      revision,
      now,
    )
    .await?;
  }
  work.execute(sql(
    "INSERT INTO message_appointments(inbox_message_id,appointment_id) VALUES ($1,$2) ON CONFLICT DO NOTHING",
    vec![inbox.into(), current.id.into()],
  )).await?;
  set_conversation_idle(&work, request.contact, now).await?;
  let local = store::local_time(preflight.choice.starts_at, details.timezone.name());
  let text = if event_id.is_some() {
    format!("Su solicitud de cambio para el {local} está pendiente de actualización en Google Calendar. Aún no puedo confirmar el nuevo horario.")
  } else {
    let state = match current.payment.as_str() {
      "awaiting_receipt" => "Su pago sigue pendiente de comprobante",
      "under_review" => "Su comprobante sigue en revisión",
      "verified" => "Su pago sigue verificado",
      "rejected" => "El estado de pago sigue como rechazado",
      _ => "La cita sigue sin requerir pago",
    };
    format!("Actualicé la fecha de su cita al {local}. {state}; la fecha límite de pago no cambia.")
  };
  enqueue_text(
    &work,
    Message {
      contact: request.contact,
      appointment: Some(current.id),
      key: &format!("appointment:{}:{}:{}", current.id, revision, "rescheduled"),
      text: &text,
      purpose: "status",
      revision: Some(revision),
    },
    origin,
    now,
  )
  .await?;
  work.commit().await?;
  Ok(Commit::Applied {
    appointment_id: current.id,
    status: status.into(),
    revision,
  })
}

pub async fn request_reschedule(
  tx: &DatabaseTransaction,
  appointment_id: Uuid,
  origin: &str,
  now: i64,
) -> Result<bool, DbErr> {
  let Some(owner) = tx
    .query_one(sql(
      "SELECT contact_id FROM appointments WHERE id=$1",
      vec![appointment_id.into()],
    ))
    .await?
  else {
    return Ok(false);
  };
  let contact: Uuid = owner.try_get("", "contact_id")?;
  tx.query_one(sql(
    "SELECT id FROM contacts WHERE id=$1 FOR UPDATE",
    vec![contact.into()],
  ))
  .await?
  .ok_or_else(|| DbErr::Custom("Appointment Contact unavailable".into()))?;
  let Some(row) = tx
    .query_one(sql(
      &format!("{APPOINTMENT_SELECT} WHERE id=$1 FOR UPDATE"),
      vec![appointment_id.into()],
    ))
    .await?
  else {
    return Ok(false);
  };
  let appointment = Appointment::from_query_result(&row, "")?;
  if !matches!(
    appointment.status.as_str(),
    "requested" | "awaiting_payment" | "confirmed"
  ) || appointment.starts_at <= now
  {
    return Ok(false);
  }
  let existing = tx
    .query_one(sql(
      "SELECT stage,data FROM conversation_state WHERE contact_id=$1 FOR UPDATE",
      vec![contact.into()],
    ))
    .await?;
  if let Some(existing) = existing {
    let stage: String = existing.try_get("", "stage")?;
    let data: serde_json::Value = existing.try_get("", "data")?;
    if stage == "reschedule_date"
      && data["appointment_id"].as_str() == Some(appointment_id.to_string().as_str())
      && data["revision"].as_i64() == Some(appointment.revision)
    {
      return Ok(false);
    }
  }
  let data = serde_json::json!({
    "appointment_id": appointment_id,
    "revision": appointment.revision,
  });
  tx.execute(sql(
    "INSERT INTO conversation_state(contact_id,stage,data,updated_at) VALUES ($1,'reschedule_date',$2,$3) ON CONFLICT (contact_id) DO UPDATE SET stage='reschedule_date',data=EXCLUDED.data,updated_at=EXCLUDED.updated_at",
    vec![contact.into(), data.into(), now.into()],
  )).await?;
  store::history(tx, appointment_id, "Doctor", "reschedule_requested", now).await?;
  let date = store::local_time(appointment.starts_at, &appointment.timezone);
  let text = format!("El doctor pidió cambiar su cita del {date}. Envíe una nueva fecha en formato AAAA-MM-DD para buscar horarios. Su cita original se conserva mientras usted elige.");
  enqueue_text(
    tx,
    Message {
      contact,
      appointment: Some(appointment_id),
      key: &format!(
        "appointment:{}:{}:{}",
        appointment_id, appointment.revision, "doctor_reschedule_request"
      ),
      text: &text,
      purpose: "status",
      revision: Some(appointment.revision),
    },
    origin,
    now,
  )
  .await?;
  Ok(true)
}

pub async fn begin_late_recovery(
  tx: &DatabaseTransaction,
  appointment_id: Uuid,
  origin: &str,
  now: i64,
) -> Result<bool, DbErr> {
  let Some(owner) = tx
    .query_one(sql(
      "SELECT contact_id FROM appointments WHERE id=$1",
      vec![appointment_id.into()],
    ))
    .await?
  else {
    return Ok(false);
  };
  let contact: Uuid = owner.try_get("", "contact_id")?;
  tx.query_one(sql(
    "SELECT id FROM contacts WHERE id=$1 FOR UPDATE",
    vec![contact.into()],
  ))
  .await?
  .ok_or_else(|| DbErr::Custom("Appointment Contact unavailable".into()))?;
  let Some(row) = tx
    .query_one(sql(
      &format!("{APPOINTMENT_SELECT} WHERE id=$1 FOR UPDATE"),
      vec![appointment_id.into()],
    ))
    .await?
  else {
    return Ok(false);
  };
  let appointment = Appointment::from_query_result(&row, "")?;
  if appointment.status != "expired"
    || appointment.payment != "verified"
    || appointment.format != "video"
  {
    return Ok(false);
  }
  let has_hold = tx
    .query_one(sql(
      "SELECT id FROM appointment_holds WHERE appointment_id=$1 AND active LIMIT 1",
      vec![appointment_id.into()],
    ))
    .await?
    .is_some();
  if has_hold {
    return Ok(false);
  }
  let data = serde_json::json!({
    "appointment_id": appointment_id,
    "revision": appointment.revision,
    "late_recovery": true,
    "format": "video",
  });
  tx.execute(sql(
    "INSERT INTO conversation_state(contact_id,stage,data,updated_at) VALUES ($1,'late_recovery_date',$2,$3) ON CONFLICT (contact_id) DO UPDATE SET stage='late_recovery_date',data=EXCLUDED.data,updated_at=EXCLUDED.updated_at",
    vec![contact.into(), data.into(), now.into()],
  )).await?;
  let text = "El doctor confirmó que recibió el pago. La cita anterior venció y no se ha reservado otro horario. Envíe una nueva fecha en formato AAAA-MM-DD para buscar opciones; no se iniciará otra solicitud de pago.";
  enqueue_text(
    tx,
    Message {
      contact,
      appointment: Some(appointment_id),
      key: &format!(
        "appointment:{}:{}:{}",
        appointment_id, appointment.revision, "late_recovery_started"
      ),
      text,
      purpose: "status",
      revision: Some(appointment.revision),
    },
    origin,
    now,
  )
  .await?;
  Ok(true)
}

async fn target_matches(
  tx: &DatabaseTransaction,
  target: &Target,
  require_video: bool,
) -> Result<bool, DbErr> {
  let Some(row) = tx
    .query_one(sql(
      "SELECT booking_calendar,generation,selection_version,status,capability_checked_at,permission_checked_at FROM google_connection WHERE id=1 FOR SHARE",
      vec![],
    ))
    .await?
  else {
    return Ok(false);
  };
  let calendar: Option<String> = row.try_get("", "booking_calendar")?;
  let generation: i64 = row.try_get("", "generation")?;
  let version: i64 = row.try_get("", "selection_version")?;
  Ok(
    calendar.as_deref() == Some(target.calendar.as_str())
      && generation == target.generation
      && version == target.selection_version
      && (!require_video
        || (row.try_get::<String>("", "status")? == "connected"
          && row
            .try_get::<Option<i64>>("", "capability_checked_at")?
            .is_some()
          && row
            .try_get::<Option<i64>>("", "permission_checked_at")?
            .is_some())),
  )
}

async fn other_active_hold(
  tx: &DatabaseTransaction,
  appointment: Uuid,
  start: i64,
  end: i64,
) -> Result<bool, DbErr> {
  Ok(tx
    .query_one(sql(
      "SELECT id FROM appointment_holds WHERE active AND appointment_id<>$1 AND starts_at<$3 AND ends_at>$2 LIMIT 1",
      vec![appointment.into(), start.into(), end.into()],
    ))
    .await?
    .is_some())
}

async fn new_payment_reference(tx: &DatabaseTransaction) -> Result<String, DbErr> {
  const ALPHABET: &[u8; 32] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";
  for _ in 0..8 {
    let mut bytes = [0u8; 6];
    getrandom::fill(&mut bytes).map_err(|_| DbErr::Custom("Randomness unavailable".into()))?;
    let reference: String = bytes
      .iter()
      .map(|byte| ALPHABET[usize::from(*byte & 31)] as char)
      .collect();
    let exists = tx
      .query_one(sql(
        "SELECT 1 FROM appointments WHERE payment_reference=$1",
        vec![reference.clone().into()],
      ))
      .await?
      .is_some();
    if !exists {
      return Ok(reference);
    }
  }
  Err(DbErr::Custom("Payment Reference generation failed".into()))
}

pub(crate) fn valid_patient(patient: Option<&str>) -> bool {
  patient.is_some_and(|name| {
    let name = name.trim();
    !name.is_empty() && name.chars().count() <= 120 && !name.chars().any(char::is_control)
  })
}

fn eligible_reschedule(appointment: &Appointment, recovery: bool, now: i64) -> bool {
  if recovery {
    appointment.status == "expired" && appointment.payment == "verified"
  } else {
    matches!(
      appointment.status.as_str(),
      "requested" | "awaiting_payment" | "confirmed"
    ) && appointment.starts_at > now
  }
}

pub(crate) async fn schedule_reminders(
  tx: &DatabaseTransaction,
  appointment: Uuid,
  revision: i64,
  starts_at: i64,
  now: i64,
  reminder_2h_enabled: bool,
) -> Result<(), DbErr> {
  let due = starts_at.saturating_sub(86_400_000);
  if due > now {
    store::job(
      tx,
      appointment,
      "reminders",
      due,
      &format!("{appointment}:{revision}:reminders:24h:{starts_at}"),
    )
    .await?;
  }
  if reminder_2h_enabled {
    let due = starts_at.saturating_sub(7_200_000);
    if due > now {
      store::job(
        tx,
        appointment,
        "reminders",
        due,
        &format!("{appointment}:{revision}:reminders:2h:{starts_at}"),
      )
      .await?;
    }
  }
  Ok(())
}

async fn set_conversation_idle(
  tx: &DatabaseTransaction,
  contact: Uuid,
  now: i64,
) -> Result<(), DbErr> {
  tx.execute(sql(
    "INSERT INTO conversation_state(contact_id,stage,data,updated_at) VALUES ($1,'idle','{}'::jsonb,$2) ON CONFLICT (contact_id) DO UPDATE SET stage='idle',data='{}'::jsonb,updated_at=EXCLUDED.updated_at",
    vec![contact.into(), now.into()],
  )).await?;
  Ok(())
}

pub(crate) struct PaymentInstructions<'a> {
  pub appointment: Uuid,
  pub revision: i64,
  pub contact: Uuid,
  pub deadline: i64,
  pub price_centavos: i64,
  pub bank: &'a str,
  pub beneficiary: &'a str,
  pub clabe: &'a str,
  pub reference: &'a str,
  pub timezone: &'a str,
}

pub(crate) async fn enqueue_payment_instructions(
  tx: &DatabaseTransaction,
  instructions: PaymentInstructions<'_>,
  origin: &str,
  now: i64,
) -> Result<(), DbErr> {
  let PaymentInstructions {
    appointment,
    revision,
    contact,
    deadline,
    price_centavos,
    bank,
    beneficiary,
    clabe,
    reference,
    timezone,
  } = instructions;
  let local_deadline = store::local_time(deadline, timezone);
  let amount = format!("${}.{:02} MXN", price_centavos / 100, price_centavos % 100);
  let text = format!("Para su cita por video: banco {bank}; beneficiario {beneficiary}; monto {amount}; envíe su comprobante antes del {local_deadline} ({timezone}). Su cita aún no está confirmada.");
  enqueue_text(
    tx,
    Message {
      contact,
      appointment: Some(appointment),
      key: &format!(
        "appointment:{}:{}:{}",
        appointment, revision, "payment_instructions"
      ),
      text: &text,
      purpose: "status",
      revision: Some(revision),
    },
    origin,
    now,
  )
  .await?;
  enqueue_text(
    tx,
    Message {
      contact,
      appointment: Some(appointment),
      key: &format!("appointment:{appointment}:{revision}:clabe"),
      text: clabe,
      purpose: "payment_details",
      revision: Some(revision),
    },
    origin,
    now,
  )
  .await?;
  enqueue_text(
    tx,
    Message {
      contact,
      appointment: Some(appointment),
      key: &format!("appointment:{appointment}:{revision}:payment_reference"),
      text: reference,
      purpose: "payment_details",
      revision: Some(revision),
    },
    origin,
    now,
  )
  .await?;
  let reference_text = "Escriba la referencia que le envié en el concepto de la transferencia.";
  enqueue_text(
    tx,
    Message {
      contact,
      appointment: Some(appointment),
      key: &format!(
        "appointment:{}:{}:{}",
        appointment, revision, "payment_reference_help"
      ),
      text: reference_text,
      purpose: "status",
      revision: Some(revision),
    },
    origin,
    now,
  )
  .await
}
