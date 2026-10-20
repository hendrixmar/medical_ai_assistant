use super::{
  crypto,
  store::{self, sql},
  Error,
};
use crate::{
  app::AppState,
  database::Db,
  modules::doctor::{Clock, DoctorSession},
};
use sea_orm::{ConnectionTrait, QueryResult};
use uuid::Uuid;

pub(super) async fn start(
  state: &AppState,
  session: &DoctorSession,
  generation: i64,
  version: i64,
  expected: Option<Uuid>,
) -> Result<(), Error> {
  let config = state.cfg.google.as_ref().ok_or(Error::Unavailable)?;
  let tx = store::transaction(&state.db).await?;
  store::live(&tx, &session.hash, state.clock.now_millis()).await?;
  let row = store::lock(&tx).await?;
  let now = state.clock.now_millis();
  if row.try_get::<String>("", "status")? != "connected"
    || row.try_get::<i64>("", "generation")? != generation
    || row.try_get::<i64>("", "selection_version")? != version
    || row.try_get::<Option<Uuid>>("", "current_probe_id")? != expected
  {
    return Err(Error::Invalid);
  }
  store::decrypt(&row, config)?;
  let calendar = row
    .try_get::<Option<String>>("", "booking_calendar")?
    .ok_or(Error::Invalid)?;
  let label = row
    .try_get::<Option<String>>("", "booking_label")?
    .ok_or(Error::Invalid)?;
  let active = tx
    .query_one(sql(
      "SELECT count(*) AS n FROM google_probes WHERE phase NOT IN ('complete','attention')",
      vec![],
    ))
    .await?
    .ok_or(Error::Unavailable)?
    .try_get::<i64>("", "n")?;
  if active > 0 {
    return Err(Error::Limited);
  }
  let starts = tx
    .query_one(sql(
      "SELECT count(*) AS n FROM google_probes WHERE started_at>$1",
      vec![(now - 3_600_000).into()],
    ))
    .await?
    .ok_or(Error::Unavailable)?
    .try_get::<i64>("", "n")?;
  if starts >= 10 {
    return Err(Error::Limited);
  }
  let id = Uuid::now_v7();
  let event_id = crypto::random::<16>()?
    .iter()
    .map(|b| format!("{b:02x}"))
    .collect::<String>();
  let conference = crypto::token()?;
  let marker = crypto::token()?;
  tx.execute(sql("INSERT INTO google_probes(id,generation,selection_version,calendar_id,calendar_label,event_id,conference_request_id,deployment_marker,probe_marker,started_at,event_start,due_at) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$10,$10)",vec![id.into(),generation.into(),version.into(),calendar.into(),label.into(),event_id.into(),conference.into(),row.try_get::<String>("","deployment_marker")?.into(),marker.into(),now.into()])).await?;
  tx.execute(sql("UPDATE google_connection SET current_probe_id=$1,permission_checked_at=NULL,capability_checked_at=NULL,notice='probe_started' WHERE id=1",vec![id.into()])).await?;
  tx.commit().await?;
  Ok(())
}
pub(super) async fn retry(
  state: &AppState,
  session: &DoctorSession,
  id: Uuid,
  generation: i64,
) -> Result<(), Error> {
  let config = state.cfg.google.as_ref().ok_or(Error::Unavailable)?;
  let tx = store::transaction(&state.db).await?;
  store::live(&tx, &session.hash, state.clock.now_millis()).await?;
  let row = store::lock(&tx).await?;
  if row.try_get::<String>("", "status")? != "connected"
    || row.try_get::<i64>("", "generation")? != generation
  {
    return Err(Error::Invalid);
  }
  store::decrypt(&row, config)?;
  let probe = tx
    .query_one(sql(
      "SELECT * FROM google_probes WHERE id=$1 FOR UPDATE",
      vec![id.into()],
    ))
    .await?
    .ok_or(Error::Invalid)?;
  let now = state.clock.now_millis();
  if probe.try_get::<String>("", "phase")? != "attention"
    || probe.try_get::<String>("", "cleanup_outcome")? != "unresolved"
    || probe
      .try_get::<Option<i64>>("", "create_committed_at")?
      .is_none()
  {
    return Err(Error::Invalid);
  }
  if probe
    .try_get::<Option<i64>>("", "last_cleanup_request")?
    .is_some_and(|t| t + 60_000 > now)
  {
    return Err(Error::Limited);
  }
  tx.execute(sql("UPDATE google_probes SET phase='cleanup',cleanup_generation=$2,cleanup_only=true,delete_auth_attempted=false,last_cleanup_request=$3,cleanup_calls=0,cleanup_deadline=$4,due_at=$3,claim=NULL,claim_deadline=NULL,reason='cleanup_requested' WHERE id=$1",vec![id.into(),generation.into(),now.into(),(now+300_000).into()])).await?;
  tx.commit().await?;
  Ok(())
}
pub(super) struct Probe {
  pub row: QueryResult,
  pub id: Uuid,
  pub claim: Uuid,
  pub deadline: i64,
  pub generation: i64,
  pub selection: i64,
  pub calendar: String,
  pub event: String,
  pub conference: String,
  pub marker: String,
  pub deployment: String,
  pub phase: String,
  pub cleanup_only: bool,
  pub authorization: i64,
}
pub(super) async fn claim(db: &Db, clock: &dyn Clock) -> Result<Option<Probe>, Error> {
  let tx = store::transaction(db).await?;
  let connection = store::lock(&tx).await?;
  let now = clock.now_millis();
  let Some(row)=tx.query_one(sql("SELECT * FROM google_probes WHERE phase NOT IN ('complete','attention') AND due_at<=$1 AND (claim IS NULL OR claim_deadline<=$1) ORDER BY due_at,started_at,id LIMIT 1 FOR UPDATE",vec![now.into()])).await? else {return Ok(None);};
  let id: Uuid = row.try_get("", "id")?;
  let generation: i64 = row.try_get("", "generation")?;
  let cleanup_only: bool = row.try_get("", "cleanup_only")?;
  let authorization = if cleanup_only {
    row
      .try_get::<Option<i64>>("", "cleanup_generation")?
      .ok_or(Error::Unavailable)?
  } else {
    generation
  };
  let phase: String = row.try_get("", "phase")?;
  let committed = row
    .try_get::<Option<i64>>("", "create_committed_at")?
    .is_some();
  let selection: i64 = row.try_get("", "selection_version")?;
  if connection.try_get::<String>("", "status")? != "connected"
    || connection.try_get::<i64>("", "generation")? != authorization
    || (phase == "prepared"
      && (connection.try_get::<i64>("", "selection_version")? != selection
        || connection.try_get::<Option<Uuid>>("", "current_probe_id")? != Some(id)))
  {
    tx.execute(sql("UPDATE google_probes SET phase=$2,cleanup_outcome=$3,reason='authorization_changed',claim=NULL,claim_deadline=NULL WHERE id=$1",vec![id.into(),if committed {"attention"} else {"complete"}.into(),if committed {"unresolved"} else {"not_created"}.into()])).await?;
    tx.commit().await?;
    return Ok(None);
  }
  let claim = Uuid::now_v7();
  let deadline = now + 30_000;
  tx.execute(sql(
    "UPDATE google_probes SET claim=$2,claim_deadline=$3 WHERE id=$1",
    vec![id.into(), claim.into(), deadline.into()],
  ))
  .await?;
  tx.commit().await?;
  Ok(Some(Probe {
    calendar: row.try_get("", "calendar_id")?,
    event: row.try_get("", "event_id")?,
    conference: row.try_get("", "conference_request_id")?,
    marker: row.try_get("", "probe_marker")?,
    deployment: row.try_get("", "deployment_marker")?,
    row,
    id,
    claim,
    deadline,
    generation,
    selection,
    phase,
    cleanup_only,
    authorization,
  }))
}
/// Last database fence before each HTTP call; budgets are spent even if a crash
/// happens before the socket write. Never return permission on uncertain COMMIT.
pub(super) async fn dispatch(
  db: &Db,
  clock: &dyn Clock,
  probe: &Probe,
  kind: &str,
) -> Result<bool, Error> {
  let tx = store::transaction(db).await?;
  let connection = store::lock(&tx).await?;
  let row = tx
    .query_one(sql(
      "SELECT * FROM google_probes WHERE id=$1 FOR UPDATE",
      vec![probe.id.into()],
    ))
    .await?
    .ok_or(Error::Unavailable)?;
  let now = clock.now_millis();
  if row.try_get::<Option<Uuid>>("", "claim")? != Some(probe.claim)
    || now + 8_000 >= probe.deadline
    || connection.try_get::<i64>("", "generation")? != probe.authorization
    || connection.try_get::<String>("", "status")? != "connected"
  {
    return Ok(false);
  }
  let phase_deadline = match kind {
    "observe" => row.try_get::<Option<i64>>("", "observation_deadline")?,
    "cleanup" => row.try_get::<Option<i64>>("", "cleanup_deadline")?,
    _ => None,
  };
  if kind == "insert" {
    if row.try_get::<String>("", "phase")? != "prepared"
      || row
        .try_get::<Option<i64>>("", "create_committed_at")?
        .is_some()
      || connection.try_get::<i64>("", "selection_version")? != probe.selection
      || connection.try_get::<Option<Uuid>>("", "current_probe_id")? != Some(probe.id)
    {
      return Ok(false);
    }
    tx.execute(sql("UPDATE google_probes SET phase='create_committed',create_committed_at=$2,observation_deadline=$3,reason='create_uncertain' WHERE id=$1",vec![probe.id.into(),now.into(),(now+120_000).into()])).await?;
  } else if kind == "observe" {
    if row.try_get::<i32>("", "observations")? >= 12
      || row
        .try_get::<Option<i64>>("", "observation_deadline")?
        .is_none_or(|t| now + 8_000 >= t)
    {
      return Ok(false);
    }
    tx.execute(sql(
      "UPDATE google_probes SET observations=observations+1 WHERE id=$1",
      vec![probe.id.into()],
    ))
    .await?;
  } else if kind == "cleanup" {
    if row.try_get::<i32>("", "cleanup_calls")? >= 12
      || row
        .try_get::<Option<i64>>("", "cleanup_deadline")?
        .is_none_or(|t| now + 8_000 >= t)
    {
      return Ok(false);
    }
    tx.execute(sql(
      "UPDATE google_probes SET cleanup_calls=cleanup_calls+1 WHERE id=$1",
      vec![probe.id.into()],
    ))
    .await?;
  }
  tx.commit().await?;
  let now = clock.now_millis();
  Ok(now + 8_000 < probe.deadline && phase_deadline.is_none_or(|t| now + 8_000 < t))
}
/// Worker-only credential authorization, under the connection → probe lock order.
/// Read the live phase: an insert 401 still carries a locally `prepared` Probe.
pub(super) async fn refresh_authorization(
  tx: &sea_orm::DatabaseTransaction,
  connection: &QueryResult,
  clock: &dyn Clock,
  probe: &Probe,
) -> Result<i64, Error> {
  let row = tx
    .query_one(sql(
      "SELECT * FROM google_probes WHERE id=$1 FOR UPDATE",
      vec![probe.id.into()],
    ))
    .await?
    .ok_or(Error::Unavailable)?;
  let phase: String = row.try_get("", "phase")?;
  let deadline = match phase.as_str() {
    "prepared" => probe.deadline,
    "create_committed" | "observing" => row
      .try_get::<Option<i64>>("", "observation_deadline")?
      .ok_or(Error::Unavailable)?,
    "cleanup" => row
      .try_get::<Option<i64>>("", "cleanup_deadline")?
      .ok_or(Error::Unavailable)?,
    _ => return Err(Error::Unavailable),
  }
  .min(probe.deadline);
  if row.try_get::<Option<Uuid>>("", "claim")? != Some(probe.claim)
    || row.try_get::<Option<i64>>("", "claim_deadline")? != Some(probe.deadline)
    || row.try_get::<i64>("", "generation")? != probe.generation
    || row.try_get::<bool>("", "cleanup_only")? != probe.cleanup_only
    || (probe.cleanup_only
      && row.try_get::<Option<i64>>("", "cleanup_generation")? != Some(probe.authorization))
    || connection.try_get::<i64>("", "generation")? != probe.authorization
    || connection.try_get::<String>("", "status")? != "connected"
    || (phase == "prepared"
      && (connection.try_get::<i64>("", "selection_version")? != probe.selection
        || connection.try_get::<Option<Uuid>>("", "current_probe_id")? != Some(probe.id)))
    || clock.now_millis() + 8_000 >= deadline
  {
    return Err(Error::Unavailable);
  }
  Ok(deadline)
}

/// Consume the single DELETE auth correction durably, before any token send.
pub(super) async fn begin_delete_correction(
  db: &Db,
  clock: &dyn Clock,
  probe: &Probe,
) -> Result<(), Error> {
  let tx = store::transaction(db).await?;
  let connection = store::lock(&tx).await?;
  refresh_authorization(&tx, &connection, clock, probe).await?;
  if tx.execute(sql("UPDATE google_probes SET delete_auth_attempted=true WHERE id=$1 AND NOT delete_auth_attempted",vec![probe.id.into()])).await?.rows_affected() != 1 {
    return Err(Error::Unavailable);
  }
  tx.commit().await?;
  Ok(())
}

pub(super) struct Evidence<'a> {
  pub phase: &'a str,
  pub reason: &'a str,
  pub conference: &'a str,
  pub cleanup: &'a str,
  pub created: bool,
  pub own: bool,
  pub read_and_deleted: bool,
  pub delay: i64,
}
pub(super) async fn record(
  db: &Db,
  clock: &dyn Clock,
  probe: &Probe,
  e: Evidence<'_>,
) -> Result<(), Error> {
  let tx = store::transaction(db).await?;
  let connection = store::lock(&tx).await?;
  let row = tx
    .query_one(sql(
      "SELECT * FROM google_probes WHERE id=$1 FOR UPDATE",
      vec![probe.id.into()],
    ))
    .await?
    .ok_or(Error::Unavailable)?;
  let now = clock.now_millis();
  if row.try_get::<Option<Uuid>>("", "claim")? != Some(probe.claim) || now >= probe.deadline {
    return Ok(());
  }
  let created = e.created || row.try_get::<bool>("", "create_evidence")?;
  let own = e.own || row.try_get::<bool>("", "own_verified")?;
  let conference = if e.conference == "pending" {
    row.try_get::<String>("", "conference_outcome")?
  } else {
    e.conference.into()
  };
  // Resolved cleanup (including tombstones) is not proof of DELETE permission.
  let permission = created && e.read_and_deleted && !probe.cleanup_only;
  tx.execute(sql("UPDATE google_probes SET phase=$2,reason=$3,conference_outcome=$4,cleanup_outcome=$5,create_evidence=$6,own_verified=$7,event_permission=$8,due_at=$9,claim=NULL,claim_deadline=NULL,cleanup_deadline=CASE WHEN $2='cleanup' THEN COALESCE(cleanup_deadline,$10) ELSE cleanup_deadline END,checked_at=CASE WHEN $2='complete' THEN $11 ELSE checked_at END WHERE id=$1",vec![probe.id.into(),e.phase.into(),e.reason.into(),conference.clone().into(),e.cleanup.into(),created.into(),own.into(),permission.into(),(now+e.delay).into(),(now+300_000).into(),now.into()])).await?;
  if e.phase == "complete"
    && !probe.cleanup_only
    && connection.try_get::<i64>("", "generation")? == probe.generation
    && connection.try_get::<i64>("", "selection_version")? == probe.selection
    && connection.try_get::<Option<Uuid>>("", "current_probe_id")? == Some(probe.id)
    && connection.try_get::<String>("", "status")? == "connected"
  {
    tx.execute(sql(
      "UPDATE google_connection SET permission_checked_at=$1,capability_checked_at=$2 WHERE id=1",
      vec![
        permission.then_some(now).into(),
        (permission && conference == "success")
          .then_some(now)
          .into(),
      ],
    ))
    .await?;
  }
  tx.commit().await?;
  Ok(())
}
