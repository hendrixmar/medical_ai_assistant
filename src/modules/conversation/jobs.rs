use super::{
  agenda, identity,
  store::{self, Message, Reply},
  templates,
};
use crate::{
  common::config::Config,
  database::Db,
  modules::{
    appointments::conversation::{self, Preflight},
    doctor::Clock,
    google_calendar::client::Client,
    whatsapp::{bound_transaction, inbox::sql},
  },
};
use sea_orm::{ConnectionTrait, DbErr, TransactionTrait};
use uuid::Uuid;

#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
  Idle,
  Processed,
}
#[derive(Debug, thiserror::Error)]
#[error("Conversation database unavailable; work remains pending")]
pub struct Unavailable;
impl From<DbErr> for Unavailable {
  fn from(_: DbErr) -> Self {
    Self
  }
}

struct Work {
  job: Uuid,
  token: Uuid,
  inbox: String,
  original: String,
  phone: Option<String>,
  envelope: serde_json::Value,
  contact: Option<Uuid>,
  doctor_hint: bool,
}

/// Provider interpretation is read-only and happens outside the final transaction.
/// Contact state, appointment effects, history, outgoing messages and job success commit together.
pub async fn run_once(
  db: &Db,
  clock: &dyn Clock,
  cfg: &Config,
  google: Option<&Client>,
) -> Result<Outcome, Unavailable> {
  let Some(origin) = cfg.public_origin.as_deref() else {
    return Ok(Outcome::Idle);
  };
  let Some(work) = claim(db, clock).await? else {
    return Ok(Outcome::Idle);
  };
  let prepared = if let Some(text) = templates::genuine_text(&work.envelope) {
    let prepare = async {
      if work.doctor_hint {
        if !agenda::needs_parser(text) {
          return Ok(None);
        }
        let Some(today) = conversation::today(db, clock.now_millis()).await? else {
          return Ok(None);
        };
        return Ok(
          super::parser::interpret_agenda(cfg, text, today)
            .await
            .map(|preference| Preflight::Agenda(preference.date)),
        );
      }
      conversation::prepare(db, clock, cfg, google, work.contact, text).await
    };
    match tokio::time::timeout(std::time::Duration::from_secs(60), prepare).await {
      Ok(Ok(prepared)) => prepared,
      _ => {
        release(db, &work).await?;
        return Err(Unavailable);
      }
    }
  } else {
    None
  };
  let result = finish(db, clock, cfg, origin, &work, prepared).await;
  if !matches!(result, Ok(Outcome::Processed)) {
    release(db, &work).await?;
  }
  result
}

async fn claim(db: &Db, clock: &dyn Clock) -> Result<Option<Work>, DbErr> {
  let tx = db.conn.begin().await?;
  bound_transaction(&tx).await?;
  let now = clock.now_millis();
  let row = tx.query_one(sql(
    "SELECT j.id,i.message_id,i.original_chat_identity,i.resolved_phone_identity,i.raw_envelope FROM processing_jobs j JOIN inbox_messages i ON i.message_id=j.inbox_message_id WHERE j.kind='process_conversation' AND j.status='pending' AND (j.claim_until IS NULL OR j.claim_until<=$1) AND j.available_at<=to_timestamp($1::double precision/1000) AND NOT EXISTS (SELECT FROM receipt_intake_state r WHERE r.inbox_message_id=i.message_id AND r.state='pending') AND NOT EXISTS (SELECT FROM inbox_messages earlier WHERE earlier.intake_order<i.intake_order AND earlier.processed_at IS NULL AND (earlier.original_chat_identity=i.original_chat_identity OR earlier.resolved_phone_identity=i.resolved_phone_identity OR earlier.original_chat_identity=i.resolved_phone_identity OR earlier.resolved_phone_identity=i.original_chat_identity OR earlier.contact_id IN (SELECT contact_id FROM contact_aliases WHERE jid IN (i.original_chat_identity,i.resolved_phone_identity)) OR EXISTS (SELECT FROM contact_aliases a JOIN contact_aliases b ON a.contact_id=b.contact_id WHERE a.jid IN (earlier.original_chat_identity,earlier.resolved_phone_identity) AND b.jid IN (i.original_chat_identity,i.resolved_phone_identity)))) ORDER BY i.intake_order FOR UPDATE OF j SKIP LOCKED LIMIT 1",
    vec![now.into()],
  )).await?;
  let Some(row) = row else {
    tx.commit().await?;
    return Ok(None);
  };
  let job: Uuid = row.try_get("", "id")?;
  let inbox: String = row.try_get("", "message_id")?;
  let original: String = row.try_get("", "original_chat_identity")?;
  let phone: Option<String> = row.try_get("", "resolved_phone_identity")?;
  let raw: Vec<u8> = row.try_get("", "raw_envelope")?;
  let envelope = serde_json::from_slice(&raw)
    .map_err(|_| DbErr::Custom("Stored conversation is unavailable".into()))?;
  let aliases = tx
    .query_all(sql(
      "SELECT DISTINCT a.contact_id,EXISTS(SELECT FROM doctor_number_state d WHERE d.id=1 AND d.contact_id=a.contact_id AND d.verified_phone IS NOT NULL) AS doctor_hint FROM contact_aliases a WHERE a.jid=$1 OR a.jid=$2 ORDER BY a.contact_id",
      vec![original.clone().into(), phone.clone().into()],
    ))
    .await?;
  let contact = if aliases.len() == 1 {
    Some(aliases[0].try_get("", "contact_id")?)
  } else {
    None
  };
  let doctor_hint = contact.is_some() && aliases[0].try_get::<bool>("", "doctor_hint")?;
  let token = Uuid::now_v7();
  tx.execute(sql(
    "UPDATE processing_jobs SET claim_token=$2,claim_until=$3 WHERE id=$1",
    vec![job.into(), token.into(), now.saturating_add(120_000).into()],
  ))
  .await?;
  tx.commit().await?;
  Ok(Some(Work {
    job,
    token,
    inbox,
    original,
    phone,
    envelope,
    contact,
    doctor_hint,
  }))
}

async fn release(db: &Db, work: &Work) -> Result<(), DbErr> {
  db.conn
    .execute(sql(
      "UPDATE processing_jobs SET claim_token=NULL,claim_until=NULL WHERE id=$1 AND claim_token=$2",
      vec![work.job.into(), work.token.into()],
    ))
    .await?;
  Ok(())
}

async fn finish(
  db: &Db,
  clock: &dyn Clock,
  cfg: &Config,
  origin: &str,
  work: &Work,
  mut preflight: Option<Preflight>,
) -> Result<Outcome, Unavailable> {
  let tx = db.conn.begin().await?;
  bound_transaction(&tx).await?;
  let lease = tx.query_one(sql(
    "SELECT claim_until FROM processing_jobs WHERE id=$1 AND kind='process_conversation' AND status='pending' AND claim_token=$2 AND claim_until>$3 AND NOT EXISTS (SELECT FROM receipt_intake_state WHERE inbox_message_id=$4 AND state='pending') FOR UPDATE",
    vec![work.job.into(), work.token.into(), clock.now_millis().into(), work.inbox.clone().into()],
  )).await?;
  let Some(lease) = lease else {
    tx.commit().await?;
    return Ok(Outcome::Idle);
  };
  let template = templates::classify(&work.envelope);
  let mut ignored = template.is_none();
  if let Some(template) = template {
    let contact = identity::assign(
      &tx,
      &work.inbox,
      &work.original,
      work.phone.as_deref(),
      clock.now_millis(),
    )
    .await?;
    let earlier = tx.query_one(sql(
      "SELECT 1 FROM inbox_messages WHERE intake_order<(SELECT intake_order FROM inbox_messages WHERE message_id=$1) AND processed_at IS NULL AND (contact_id=$2 OR original_chat_identity IN (SELECT jid FROM contact_aliases WHERE contact_id=$2) OR resolved_phone_identity IN (SELECT jid FROM contact_aliases WHERE contact_id=$2)) LIMIT 1",
      vec![work.inbox.clone().into(), contact.into()],
    )).await?.is_some();
    if earlier || clock.now_millis() >= lease.try_get::<i64>("", "claim_until")? {
      tx.rollback().await?;
      return Ok(Outcome::Idle);
    }
    let conflict = tx
      .query_one(sql(
        "SELECT identity_conflict FROM contacts WHERE id=$1",
        vec![contact.into()],
      ))
      .await?
      .ok_or(Unavailable)?
      .try_get::<bool>("", "identity_conflict")?;
    let receipt = tx
      .query_one(sql(
        "SELECT 1 FROM receipt_intake_state WHERE inbox_message_id=$1 AND state='complete'",
        vec![work.inbox.clone().into()],
      ))
      .await?
      .is_some();
    if conflict || receipt {
      ignored = true;
    } else {
      let text = templates::genuine_text(&work.envelope);
      let doctor = crate::modules::doctor_number::store::recognize(
        &tx,
        clock,
        cfg.doctor_totp_key.as_ref(),
        crate::modules::doctor_number::store::Incoming {
          contact,
          inbox: &work.inbox,
          original: &work.original,
          phone: work.phone.as_deref(),
          envelope: &work.envelope,
          text,
        },
      )
      .await
      .map_err(|_| Unavailable)?;
      let now = clock.now_millis();
      if doctor == Some("doctor_read_only") && text.is_some() {
        let question = match preflight.as_ref() {
          Some(Preflight::Agenda(date)) if work.contact == Some(contact) => {
            agenda::Question::Date(*date)
          }
          _ => agenda::Question::Text(text.unwrap_or_default()),
        };
        if let Some(answer) = agenda::answer(&tx, now, question).await? {
          store::enqueue_text(
            &tx,
            Message {
              contact,
              appointment: None,
              key: &format!("doctor-agenda:{}", work.inbox),
              text: &answer,
              purpose: "doctor_notification",
              revision: None,
            },
            origin,
            now,
          )
          .await?;
        } else {
          enqueue(&tx, work, contact, "doctor_read_only", origin, now).await?;
        }
      } else if let Some(doctor) = doctor {
        let template = if doctor == "doctor_read_only" && template == "audio" {
          "doctor_audio"
        } else {
          doctor
        };
        enqueue(&tx, work, contact, template, origin, now).await?;
      } else if crate::modules::whatsapp::takeover::active(&tx, contact, now).await? {
        ignored = true;
      } else if let Some(text) = text {
        if journeys_available(&tx, contact).await? {
          if work.contact.is_some_and(|prepared| prepared != contact) {
            preflight = None;
          }
          conversation::handle(
            &conversation::Turn {
              tx: &tx,
              contact,
              inbox: &work.inbox,
              original: &work.original,
              origin,
              now,
            },
            text,
            preflight,
          )
          .await?;
        } else {
          let template = if text.eq_ignore_ascii_case("DOCTOR") {
            "followup"
          } else {
            template
          };
          if template == "followup" {
            tx.execute(sql(
              "INSERT INTO followup_requests(id,contact_id,inbox_message_id,original_target,created_at) VALUES ($1,$2,$3,$4,$5) ON CONFLICT (contact_id) WHERE resolved_at IS NULL DO NOTHING",
              vec![Uuid::now_v7().into(), contact.into(), work.inbox.clone().into(), work.original.clone().into(), now.into()],
            )).await?;
          }
          enqueue(&tx, work, contact, template, origin, now).await?;
        }
      } else {
        enqueue(&tx, work, contact, template, origin, now).await?;
      }
    }
  }
  let now = clock.now_millis();
  tx.execute(sql(
    "UPDATE inbox_messages SET processed_at=$2,conversation_ignored=$3 WHERE message_id=$1",
    vec![work.inbox.clone().into(), now.into(), ignored.into()],
  ))
  .await?;
  tx.execute(sql(
    "UPDATE processing_jobs SET status='succeeded',finished_at=to_timestamp($3::double precision/1000),claim_token=NULL,claim_until=NULL WHERE id=$1 AND claim_token=$2",
    vec![work.job.into(), work.token.into(), now.into()],
  )).await?;
  tx.commit().await?;
  Ok(Outcome::Processed)
}

async fn enqueue(
  tx: &sea_orm::DatabaseTransaction,
  work: &Work,
  contact: Uuid,
  template: &str,
  origin: &str,
  now: i64,
) -> Result<(), DbErr> {
  store::enqueue(
    tx,
    Reply {
      contact,
      inbox: &work.inbox,
      target: &work.original,
      followup: None,
      template,
    },
    origin,
    now,
  )
  .await
}

async fn journeys_available(
  tx: &sea_orm::DatabaseTransaction,
  contact: Uuid,
) -> Result<bool, DbErr> {
  let row = tx.query_one(sql(
    "SELECT (EXISTS (SELECT FROM appointments WHERE contact_id=$1) OR (location IS NOT NULL AND timezone IS NOT NULL AND weekly_hours IS NOT NULL AND buffer_before_minutes IS NOT NULL AND buffer_after_minutes IS NOT NULL AND minimum_notice_minutes IS NOT NULL AND cancellation_policy IS NOT NULL AND EXISTS (SELECT FROM services))) AS available FROM practice WHERE id=1",
    vec![contact.into()],
  )).await?;
  row
    .map(|row| row.try_get("", "available"))
    .transpose()
    .map(|value| value.unwrap_or(false))
}
