use crate::modules::doctor::views::{document, escape, navigation_page, NavigationPage};
use chrono::{DateTime, SecondsFormat, Utc};
use uuid::Uuid;

pub(crate) struct Candidate {
  pub appointment_id: Uuid,
  pub patient_name: String,
  pub service_name: String,
  pub starts_at: i64,
  pub timezone: String,
}

pub(crate) struct AppointmentSummary {
  pub id: Uuid,
  pub patient_name: String,
  pub service_name: String,
  pub starts_at: i64,
  pub timezone: String,
  pub status: String,
  pub payment: String,
  pub can_verify: bool,
  pub can_late_review: bool,
  pub can_cancel: bool,
}

pub(crate) struct ReceiptReview {
  pub id: Uuid,
  pub mime: String,
  pub sent_at: i64,
  pub arrived_at: i64,
  pub timely: Option<bool>,
  pub late: Option<bool>,
  pub bytes_available: bool,
  pub reviewed_at: Option<i64>,
  pub review_outcome: Option<String>,
  pub clarification_requested_at: Option<i64>,
  pub appointment: Option<AppointmentSummary>,
  pub candidates: Vec<Candidate>,
}

pub(crate) fn receipt(csrf: &str, view: &ReceiptReview) -> String {
  let id = escape(&view.id.to_string());
  let timely = match view.timely {
    Some(true) => "Enviado a tiempo según WhatsApp",
    Some(false) => "Enviado después del plazo según WhatsApp",
    None => "Pendiente de selección de cita",
  };
  let late = match view.late {
    Some(true) => "Llegó después del vencimiento del horario",
    Some(false) => "Llegó antes del vencimiento del horario",
    None => "Pendiente de selección de cita",
  };
  let outcome = view
    .review_outcome
    .as_deref()
    .map(review_outcome)
    .unwrap_or("Pendiente de decisión del doctor");
  let evidence = if view.bytes_available {
    format!(
      r#"<p>Archivo protegido disponible: {mime}</p>
<a class="primary-button" href="/dashboard/payments/receipts/{id}/file">Descargar comprobante</a>"#,
      mime = escape(&view.mime),
    )
  } else {
    "<p class=\"form-error\" role=\"status\">El archivo ya no está disponible. No es posible verificarlo con esta copia; solicite que el Contact lo envíe otra vez.</p>".to_owned()
  };
  let appointment = view
    .appointment
    .as_ref()
    .map(appointment_details)
    .unwrap_or_else(|| selection(&id, csrf, view));
  let actions = view
    .appointment
    .as_ref()
    .map(|appointment| action_forms(&id, csrf, view, appointment))
    .unwrap_or_default();
  let body = format!(
    r#"<main id="contenido" class="page-shell" tabindex="-1">
<h1>Revisión de comprobante</h1>
<p>La imagen o PDF es evidencia de transferencia, no confirmación de fondos. Sólo la decisión expresa del doctor puede verificar el pago.</p>
<section class="dashboard-section" aria-labelledby="evidence-heading">
<h2 id="evidence-heading">Archivo</h2>
{evidence}
<dl class="appointment-facts">
<div><dt>Tipo</dt><dd>{mime}</dd></div>
<div><dt>Enviado</dt><dd>{sent}</dd></div>
<div><dt>Recibido por la aplicación</dt><dd>{arrived}</dd></div>
<div><dt>Plazo según WhatsApp</dt><dd>{timely}</dd></div>
<div><dt>Vencimiento del horario</dt><dd>{late}</dd></div>
<div><dt>Decisión</dt><dd>{outcome}</dd></div>
</dl>
</section>
{appointment}
{actions}
<p><a href="/appointments">Volver a citas</a></p>
</main>"#,
    evidence = evidence,
    mime = escape(&view.mime),
    sent = escape(&timestamp(view.sent_at)),
    arrived = escape(&timestamp(view.arrived_at)),
    timely = escape(timely),
    late = escape(late),
    outcome = escape(outcome),
    appointment = appointment,
    actions = actions,
  );
  document(
    "Revisión de comprobante",
    &format!(
      "{}{}",
      navigation_page(csrf, NavigationPage::Appointments),
      body
    ),
  )
}

fn appointment_details(appointment: &AppointmentSummary) -> String {
  format!(
    r#"<section class="dashboard-section" aria-labelledby="appointment-heading">
<h2 id="appointment-heading">Cita asociada</h2>
<dl class="appointment-facts">
<div><dt>Paciente</dt><dd>{patient}</dd></div>
<div><dt>Servicio</dt><dd>{service}</dd></div>
<div><dt>Horario</dt><dd>{starts}</dd></div>
<div><dt>Estado</dt><dd>{status}</dd></div>
<div><dt>Pago</dt><dd>{payment}</dd></div>
</dl>
<a href="/appointments/{id}">Abrir cita</a>
</section>"#,
    patient = escape(&appointment.patient_name),
    service = escape(&appointment.service_name),
    starts = escape(&local_time(appointment.starts_at, &appointment.timezone)),
    status = escape(status(&appointment.status)),
    payment = escape(payment(&appointment.payment)),
    id = escape(&appointment.id.to_string()),
  )
}

fn selection(id: &str, csrf: &str, view: &ReceiptReview) -> String {
  if view.candidates.is_empty() {
    return "<section><h2>Cita</h2><p>No hay una cita asociada a este comprobante.</p></section>"
      .to_owned();
  }
  let mut options = String::from("<label for=\"appointment_id\">Elija la cita correcta</label><select id=\"appointment_id\" name=\"appointment_id\" required>");
  for candidate in &view.candidates {
    options.push_str(&format!(
      "<option value=\"{}\">{} · {} · {}</option>",
      escape(&candidate.appointment_id.to_string()),
      escape(&candidate.patient_name),
      escape(&candidate.service_name),
      escape(&local_time(candidate.starts_at, &candidate.timezone)),
    ));
  }
  options.push_str("</select>");
  format!(
    r#"<section aria-labelledby="selection-heading">
<h2 id="selection-heading">Elija la cita</h2>
<p>Hay más de una cita con pago pendiente. El comprobante no se asociará hasta que usted seleccione explícitamente la correcta.</p>
<form method="post" action="/dashboard/payments/receipts/{id}/association">
<input type="hidden" name="csrf_token" value="{csrf}">
{options}
<button class="primary-button" type="submit">Asociar comprobante</button>
</form>
</section>"#,
    id = escape(id),
    csrf = escape(csrf),
  )
}

fn action_forms(
  id: &str,
  csrf: &str,
  view: &ReceiptReview,
  appointment: &AppointmentSummary,
) -> String {
  if view.reviewed_at.is_some() || !view.bytes_available {
    return String::new();
  }
  let mut forms = String::from("<section class=\"dashboard-section\" aria-labelledby=\"actions-heading\"><h2 id=\"actions-heading\">Acciones del doctor</h2><div class=\"appointment-actions\">");
  if appointment.status == "expired" && appointment.can_late_review {
    forms.push_str(&format!(
      r#"<form method="post" action="/dashboard/payments/receipts/{id}/late-outcome">
<input type="hidden" name="csrf_token" value="{csrf}">
<input type="hidden" name="outcome" value="verified_recovery">
<button class="primary-button" type="submit">Pago verificado: ofrecer nuevos horarios</button>
</form>
<form method="post" action="/dashboard/payments/receipts/{id}/late-outcome">
<input type="hidden" name="csrf_token" value="{csrf}">
<input type="hidden" name="outcome" value="verified_refund">
<button type="submit">Pago verificado: mantener vencida y gestionar reembolso manual</button>
</form>"#,
      id = escape(id),
      csrf = escape(csrf),
    ));
    if view.clarification_requested_at.is_none() {
      forms.push_str(&format!(
        r#"<form method="post" action="/dashboard/payments/receipts/{id}/late-outcome">
<input type="hidden" name="csrf_token" value="{csrf}">
<input type="hidden" name="outcome" value="clarification">
<button type="submit">Fondos ausentes o poco claros: pedir aclaración</button>
</form>"#,
        id = escape(id),
        csrf = escape(csrf),
      ));
    } else {
      forms.push_str("<p>Ya se solicitó una aclaración. La cita sigue vencida; aún puede registrar una decisión expresa sobre los fondos.</p>");
    }
  } else if appointment.status == "expired" {
    forms.push_str("<p>Esta cita venció y ya no tiene una decisión de pago pendiente. No hay un horario reservado.</p>");
  } else if appointment.can_verify {
    forms.push_str(&format!(
      r#"<form method="post" action="/dashboard/payments/receipts/{id}/verify">
<input type="hidden" name="csrf_token" value="{csrf}">
<button class="primary-button" type="submit">Confirmar pago recibido</button>
</form>
<form method="post" action="/dashboard/payments/receipts/{id}/reject">
<input type="hidden" name="csrf_token" value="{csrf}">
<input type="hidden" name="choice" value="receipt">
<button type="submit">No recibí el pago: pedir nuevo comprobante</button>
</form>
<form method="post" action="/dashboard/payments/receipts/{id}/reject">
<input type="hidden" name="csrf_token" value="{csrf}">
<input type="hidden" name="choice" value="cancel">
<button type="submit">No recibí el pago: cancelar la cita</button>
</form>"#,
      id = escape(id),
      csrf = escape(csrf),
    ));
  }
  if appointment.can_cancel {
    forms.push_str(&format!(
      r#"<form method="post" action="/dashboard/payments/receipts/{id}/cancel">
<input type="hidden" name="csrf_token" value="{csrf}">
<button type="submit">Cancelar cita</button>
</form>"#,
      id = escape(id),
      csrf = escape(csrf),
    ));
  }
  forms.push_str("</div></section>");
  forms
}

fn timestamp(value: i64) -> String {
  DateTime::<Utc>::from_timestamp_millis(value)
    .map(|value| value.to_rfc3339_opts(SecondsFormat::Secs, true))
    .unwrap_or_else(|| "fecha no disponible".to_owned())
}

fn local_time(value: i64, timezone: &str) -> String {
  let Ok(timezone) = timezone.parse::<chrono_tz::Tz>() else {
    return "horario no disponible".to_owned();
  };
  DateTime::<Utc>::from_timestamp_millis(value)
    .map(|time| {
      time
        .with_timezone(&timezone)
        .format("%Y-%m-%d %H:%M %Z")
        .to_string()
    })
    .unwrap_or_else(|| "horario no disponible".to_owned())
}

fn status(value: &str) -> &'static str {
  match value {
    "requested" => "solicitada",
    "awaiting_payment" => "pendiente de pago",
    "confirmed" => "confirmada",
    "cancelled" => "cancelada",
    "expired" => "vencida",
    _ => "estado no reconocido",
  }
}

fn payment(value: &str) -> &'static str {
  match value {
    "not_required" => "no requerido",
    "awaiting_receipt" => "pendiente de comprobante",
    "under_review" => "en revisión",
    "verified" => "verificado por el doctor",
    "rejected" => "rechazado por el doctor",
    _ => "estado no reconocido",
  }
}

fn review_outcome(value: &str) -> &'static str {
  match value {
    "verified" => "Pago verificado por el doctor",
    "rejected" => "Pago rechazado por el doctor",
    "new_receipt" => "Se solicitó un nuevo comprobante",
    "cancelled" => "Cita cancelada por el doctor",
    "late_verified_recovery" => "Pago tardío verificado; se ofreció recuperar la cita",
    "late_verified_refund" => "Pago tardío verificado; reembolso manual pendiente",
    "late_clarification" => "Se solicitó aclarar el comprobante tardío",
    _ => "Decisión registrada",
  }
}
