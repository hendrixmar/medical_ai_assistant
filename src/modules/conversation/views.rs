use super::history;
use crate::modules::doctor::views::{document, escape, navigation_page, NavigationPage};
use uuid::Uuid;
#[derive(Clone, Copy)]
pub enum GoogleConnectionStatus {
  NotConfigured,
  Disconnected,
  ReauthorizationRequired,
  Connected,
  Unavailable,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum AttentionKind {
  Followup,
  ReceiptFollowup,
  SendFailed,
  SendUncertain,
  SendErrorReported,
  IdentityConflict,
  ReceiptReview,
  ReceiptCopyFailed,
  LateReceipt,
  ReceiptSelection,
  ReceiptClarification,
  RefundDue,
  CalendarConflict,
  CalendarSyncFailed,
  CalendarAvailabilityUnavailable,
  CalendarCheckUnavailable,
  GoogleReauthorizationRequired,
  AvailabilityConflict,
  WhatsAppDisconnected,
  WhatsAppRecoveryIncomplete,
  WhatsAppIdentityMissing,
  GoogleDisconnected,
  RetentionPolicyReview,
  Other,
}

pub struct AttentionItem<'a> {
  pub id: &'a str,
  pub contact: &'a str,
  pub occurred_at: &'a str,
  pub kind: AttentionKind,
  pub contact_id: Option<&'a str>,
  pub appointment_id: Option<&'a str>,
  pub receipt_id: Option<&'a str>,
}

pub struct DashboardAppointment<'a> {
  pub id: &'a str,
  pub patient_name: &'a str,
  pub service_name: &'a str,
  pub starts_at: &'a str,
  pub status: &'a str,
  pub payment: &'a str,
  pub calendar_sync: &'a str,
}

pub struct DashboardView<'a> {
  pub csrf_token: &'a str,
  pub items: &'a [AttentionItem<'a>],
  pub notice: Option<&'a str>,
  pub load_error: bool,
  pub appointment_load_error: bool,
  pub appointments: &'a [DashboardAppointment<'a>],
  pub google_cleanup_count: u64,
  pub google_status: GoogleConnectionStatus,
  pub google_load_error: bool,
  pub outbound_enabled: bool,
  pub queued_count: u64,
  pub rate_deferred_count: u64,
  pub previous_page: Option<u32>,
  pub next_page: Option<u32>,
}

pub fn dashboard(view: DashboardView<'_>) -> String {
  let notice = view
    .notice
    .map(|text| {
      format!(
        "<p class=\"dashboard-notice\" role=\"status\">{}</p>",
        escape(text)
      )
    })
    .unwrap_or_default();
  let outbound = if view.outbound_enabled {
    "El envío automático está habilitado. Esto no confirma la conexión de WhatsApp ni la entrega de los mensajes."
  } else {
    "El envío automático está deshabilitado. Los mensajes pendientes se conservan en cola; no se iniciarán nuevos envíos mientras siga deshabilitado."
  };
  let attention = if view.load_error {
    String::from(
      r#"<div class="form-error" role="alert" tabindex="-1" autofocus>
<p>No se pudieron cargar todas las solicitudes y problemas pendientes. No podemos confirmar si hay asuntos que necesitan tu atención.</p>
<a href="/dashboard">Volver a cargar el panel</a>
</div>"#,
    )
  } else {
    attention_content(&view)
  };
  let appointments = if view.appointment_load_error {
    String::from(
      r#"<div class="form-error" role="alert" tabindex="-1" autofocus>
<p>No se pudieron cargar las citas próximas. No podemos confirmar si hay citas pendientes.</p>
<a href="/dashboard">Volver a cargar el panel</a>
</div>"#,
    )
  } else {
    dashboard_appointments(&view)
  };
  document(
    "Tu consultorio",
    &format!(
      r#"{}
<main id="contenido" class="page-shell" tabindex="-1">
<h1>Tu consultorio</h1>
<p class="intro">Revisa solicitudes de atención, problemas de mensajes, comprobantes, citas y conexiones del consultorio.</p>
{notice}
<section class="dashboard-section attention-section" aria-labelledby="attention-title">
<h2 id="attention-title">Necesita tu atención</h2>
{}
<p class="muted">{outbound}</p>
{attention}
</section>
<section class="dashboard-section" aria-labelledby="appointments-title">
<h2 id="appointments-title">Tus próximas citas</h2>
{appointments}
</section>
</main>"#,
      navigation_page(view.csrf_token, NavigationPage::Dashboard),
      google_attention(&view)
    ),
  )
}

fn dashboard_appointments(view: &DashboardView<'_>) -> String {
  if view.appointments.is_empty() {
    return "<p class=\"empty-title\">No hay citas próximas en este momento.</p><a class=\"text-link\" href=\"/appointments\">Ver citas próximas y pasadas</a>".to_owned();
  }
  let mut content = String::from("<ul class=\"appointment-list\" role=\"list\">");
  for appointment in view.appointments {
    content.push_str(&format!(
      r#"<li>
<h3><a href="/appointments/{}">Cita de {}</a></h3>
<p>{}</p>
<p class="appointment-time">{}</p>
<p class="muted">Estado: {}. Pago: {}. Calendario: {}.</p>
</li>"#,
      escape(appointment.id),
      escape(appointment.patient_name),
      escape(appointment.service_name),
      escape(appointment.starts_at),
      escape(appointment.status),
      escape(appointment.payment),
      escape(appointment.calendar_sync)
    ));
  }
  content.push_str("</ul><a class=\"text-link\" href=\"/appointments\">Ver todas las citas</a>");
  content
}

fn google_attention(view: &DashboardView<'_>) -> String {
  let connection = match view.google_status {
    GoogleConnectionStatus::NotConfigured => {
      "<p>Google Calendar no está configurado en esta instalación.</p>"
    }
    GoogleConnectionStatus::Disconnected => {
      "<p>No hay una conexión guardada con Google Calendar.</p>"
    }
    GoogleConnectionStatus::ReauthorizationRequired => {
      "<p>Google Calendar requiere volver a conectarse.</p>"
    }
    GoogleConnectionStatus::Connected => "",
    GoogleConnectionStatus::Unavailable => {
      "<p>No se pudo confirmar el estado guardado de Google Calendar.</p>"
    }
  };
  let cleanup = if view.google_load_error {
    "<p role=\"alert\">No se pudo cargar todo el estado guardado de Google Calendar; los elementos pendientes no se consideran resueltos.</p>"
      .to_owned()
  } else if view.google_cleanup_count > 0 {
    format!(
      "<p>Eventos de prueba de Google con limpieza pendiente: {}.</p><p class=\"muted\">Se conservan aunque otra prueba termine bien. Revisa el calendario original y las opciones de limpieza en la configuración.</p>",
      view.google_cleanup_count
    )
  } else {
    String::new()
  };
  if connection.is_empty() && cleanup.is_empty() {
    return String::new();
  }
  format!(
    "<div class=\"google-attention\" aria-labelledby=\"google-attention-title\"><h3 id=\"google-attention-title\">Google Calendar</h3>{connection}{cleanup}<a class=\"text-link\" href=\"/settings/google\">Revisar Google Calendar</a></div>"
  )
}

fn attention_content(view: &DashboardView<'_>) -> String {
  let mut content = format!(
    "<p class=\"queue-summary\">Mensajes en cola: {}. Aplazados por el límite de respuestas por contacto: {}.</p>",
    view.queued_count, view.rate_deferred_count
  );
  if view.items.is_empty() {
    if view.previous_page.is_some()
      || view.next_page.is_some()
      || view.google_cleanup_count > 0
      || view.google_load_error
    {
      content.push_str(
        "<p class=\"empty-title\">No hay otros elementos de atención en esta página.</p>",
      );
    } else {
      content.push_str("<p class=\"empty-title\">No hay solicitudes ni problemas operativos pendientes en esta lista.</p>");
    }
  } else {
    content.push_str("<ul class=\"attention-list\" role=\"list\">");
    for (index, item) in view.items.iter().enumerate() {
      let (title, description) = match item.kind {
        AttentionKind::Followup => (
          "Solicitud de atención personal",
          "Este contacto solicitó hablar con el doctor.",
        ),
        AttentionKind::ReceiptFollowup => (
          "Solicitud de atención relacionada con un comprobante",
          "El contacto pidió hablar con el doctor en relación con un comprobante. Revisa la cita asociada; esta solicitud no confirma el pago.",
        ),
        AttentionKind::SendFailed => (
          "Envío fallido",
          "No se confirmó el envío de este mensaje. No se reenviará automáticamente.",
        ),
        AttentionKind::SendUncertain => (
          "Envío incierto",
          "El mensaje puede haberse enviado, pero no se pudo confirmar el resultado. No se reenviará automáticamente para evitar duplicados.",
        ),
        AttentionKind::SendErrorReported => (
          "Error reportado por WhatsApp",
          "WhatsApp reportó un error con este mensaje. Esto no descarta una entrega o lectura anterior. No se reenviará automáticamente.",
        ),
        AttentionKind::IdentityConflict => (
          "Identidad del contacto en conflicto",
          "No se puede asociar este contacto de forma segura. Sus envíos están suspendidos; revisa la identidad antes de responder.",
        ),
        AttentionKind::ReceiptReview => (
          "Revisar comprobante",
          "El comprobante es evidencia de una transferencia, no confirma que el pago se haya recibido.",
        ),
        AttentionKind::ReceiptCopyFailed => (
          "No se pudo guardar el comprobante",
          "No hay una copia protegida del archivo disponible para revisar; el pago no está confirmado.",
        ),
        AttentionKind::LateReceipt => (
          "Revisar comprobante tardío",
          "El comprobante llegó después de vencer la reserva. La cita no se reactiva automáticamente.",
        ),
        AttentionKind::ReceiptSelection => (
          "Elegir cita para el comprobante",
          "Hay más de una cita posible. Revisa el comprobante y elige una asociación explícita.",
        ),
        AttentionKind::ReceiptClarification => (
          "Aclarar comprobante",
          "Se requiere una decisión del doctor; el sistema no verificó el pago.",
        ),
        AttentionKind::RefundDue => (
          "Reembolso pendiente",
          "El reembolso debe gestionarse fuera del sistema; no se inició ningún reembolso automático.",
        ),
        AttentionKind::CalendarConflict => (
          "Conflicto en un calendario",
          "Hay un conflicto registrado. La cita no se cambió automáticamente.",
        ),
        AttentionKind::CalendarSyncFailed => (
          "Sincronización de calendario pendiente",
          "No se confirmó la actualización de Google Calendar. La cita y su estado de pago se conservan.",
        ),
        AttentionKind::CalendarAvailabilityUnavailable => (
          "No se pudo consultar la disponibilidad",
          "La solicitud no está confirmada mientras la disponibilidad del calendario siga sin comprobarse.",
        ),
        AttentionKind::CalendarCheckUnavailable => (
          "No se pudo comprobar el calendario",
          "No podemos confirmar la disponibilidad ni el estado de Google Calendar para esta cita.",
        ),
        AttentionKind::GoogleReauthorizationRequired => (
          "Google Calendar requiere volver a conectarse",
          "No se pudo completar la operación mientras la conexión de Google necesita autorización.",
        ),
        AttentionKind::AvailabilityConflict => (
          "Solicitud con conflicto de disponibilidad",
          "La solicitud conserva su estado actual; no se prometió ni cambió automáticamente la hora.",
        ),
        AttentionKind::WhatsAppDisconnected => (
          "WhatsApp desconectado",
          "La conexión de WhatsApp requiere atención. Los mensajes pendientes se conservan.",
        ),
        AttentionKind::WhatsAppRecoveryIncomplete => (
          "Recuperación de WhatsApp pendiente",
          "No se completó la recuperación de mensajes. Revisa el estado de WhatsApp.",
        ),
        AttentionKind::WhatsAppIdentityMissing => (
          "Atribución de WhatsApp no confirmada",
          "La identidad o el alias no se confirmaron con seguridad. Los mensajes directos del teléfono que no puedan atribuirse no se muestran bajo ningún contacto.",
        ),
        AttentionKind::GoogleDisconnected => (
          "Google Calendar desconectado",
          "No se pudo confirmar una conexión activa con Google Calendar.",
        ),
        AttentionKind::RetentionPolicyReview => (
          "Revisar política de conservación de mensajes sin cita asociada.",
          "Hay mensajes cuya conservación necesita una decisión de política. No se muestra ni se asocia su contenido aquí.",
        ),
        AttentionKind::Other => (
          "Pendiente operativo",
          "Hay un asunto pendiente que requiere revisión.",
        ),
      };
      let mut actions = String::new();
      if matches!(
        item.kind,
        AttentionKind::Followup | AttentionKind::ReceiptFollowup
      ) {
        actions.push_str(&format!(
          r#"<form class="attention-action" method="post" action="/followups/{}/resolve" aria-labelledby="attention-contact-{index}" aria-describedby="resolution-help">
<input type="hidden" name="csrf_token" value="{}">
<button class="primary-button" type="submit">Marcar como atendida</button>
</form>"#,
          escape(item.id),
          escape(view.csrf_token)
        ));
      }
      if let Some(receipt_id) = item.receipt_id {
        actions.push_str(&format!(
          r#"<a class="text-link attention-link" href="/dashboard/payments/receipts/{}">Revisar comprobante</a>"#,
          escape(receipt_id)
        ));
      }
      if let Some(appointment_id) = item.appointment_id {
        actions.push_str(&format!(
          r#"<a class="text-link attention-link" href="/appointments/{}">Ver cita relacionada</a>"#,
          escape(appointment_id)
        ));
      }
      if let Some(contact_id) = item.contact_id {
        actions.push_str(&format!(
          r#"<a class="text-link attention-link" href="/contacts/{}">Ver conversación</a>"#,
          escape(contact_id)
        ));
      }
      match item.kind {
        AttentionKind::WhatsAppDisconnected
        | AttentionKind::WhatsAppRecoveryIncomplete
        | AttentionKind::WhatsAppIdentityMissing => {
          actions.push_str(
            r#"<a class="text-link attention-link" href="/settings/whatsapp">Revisar WhatsApp</a>"#,
          );
        }
        AttentionKind::GoogleDisconnected
        | AttentionKind::CalendarConflict
        | AttentionKind::CalendarSyncFailed
        | AttentionKind::CalendarAvailabilityUnavailable
        | AttentionKind::CalendarCheckUnavailable
        | AttentionKind::GoogleReauthorizationRequired => {
          actions.push_str(
            r#"<a class="text-link attention-link" href="/settings/google">Revisar Google Calendar</a>"#,
          );
        }
        _ => {}
      }
      content.push_str(&format!(
        r#"<li>
<h3>{title}</h3>
<p class="attention-contact" id="attention-contact-{index}">{}</p>
<p class="attention-time">Registrado: {}</p>
<p class="muted">{description}</p>
{actions}
</li>"#,
        escape(item.contact),
        escape(item.occurred_at)
      ));
    }
    content.push_str("</ul>");
    if view.items.iter().any(|item| {
      matches!(
        item.kind,
        AttentionKind::Followup | AttentionKind::ReceiptFollowup
      )
    }) {
      content.push_str(
        "<p class=\"muted\" id=\"resolution-help\">Marcar como atendida cierra la solicitud y deja un aviso en cola. No confirma que hayas hablado con el contacto ni que el aviso se haya entregado.</p>",
      );
    }
  }
  if view.previous_page.is_some() || view.next_page.is_some() {
    content.push_str("<nav class=\"attention-pagination\" aria-label=\"Páginas de pendientes\">");
    if let Some(page) = view.previous_page {
      content.push_str(&format!(
        "<a class=\"text-link\" href=\"/dashboard?page={page}\" rel=\"prev\">Página anterior</a>"
      ));
    }
    if let Some(page) = view.next_page {
      content.push_str(&format!(
        "<a class=\"text-link\" href=\"/dashboard?page={page}\" rel=\"next\">Página siguiente</a>"
      ));
    }
    content.push_str("</nav>");
  }
  content
}

pub(super) struct ContactInboxView<'a> {
  pub csrf_token: &'a str,
  pub page: u32,
  pub contacts: &'a [history::InboxContact],
  pub load_error: bool,
  pub next_page: bool,
}

pub(super) fn contact_inbox(view: ContactInboxView<'_>) -> String {
  let content = if view.load_error {
    format!(
      r#"<div class="form-error" role="alert" tabindex="-1" autofocus>
<p>No se pudieron cargar las conversaciones. No podemos confirmar si hay mensajes nuevos.</p>
<a href="/contacts?page={}">Volver a cargar las conversaciones</a>
</div>"#,
      view.page
    )
  } else if view.contacts.is_empty() {
    "<p class=\"empty-title\">No hay conversaciones registradas.</p>".to_owned()
  } else {
    let mut content = String::from("<ul class=\"contact-list\" role=\"list\">");
    for contact in view.contacts {
      let conflict = if contact.identity_conflict {
        "<p class=\"muted\">La identidad está en conflicto; revisa antes de responder.</p>"
      } else {
        ""
      };
      let followup = if contact.has_open_followup {
        "<p class=\"muted\">Hay una solicitud de atención personal pendiente.</p>"
      } else {
        ""
      };
      let takeover = contact
        .takeover_until
        .as_ref()
        .map(|until| {
          format!(
            "<p class=\"muted\">El asistente está silenciado para este contacto hasta: {}</p>",
            escape(until)
          )
        })
        .unwrap_or_default();
      content.push_str(&format!(
        r#"<li>
<h2>{}</h2>
<p class="contact-last-activity">Última actividad: {}</p>
{conflict}{followup}{takeover}
<a class="text-link contact-open" href="/contacts/{}">Ver conversación</a>
</li>"#,
        escape(&contact.label),
        escape(&contact.last_activity),
        contact.id
      ));
    }
    content.push_str("</ul>");
    content
  };
  let pagination = page_links(
    "/contacts",
    view.page,
    view.page.checked_sub(1),
    view.next_page,
  );
  document(
    "Conversaciones",
    &format!(
      r#"{}
<main id="contenido" class="page-shell" tabindex="-1">
<h1>Conversaciones</h1>
<p class="intro">Consulta el historial administrativo de cada contacto. No se crea un registro de pacientes.</p>
{content}
{pagination}
</main>"#,
      navigation_page(view.csrf_token, NavigationPage::Contacts)
    ),
  )
}

pub(super) fn contact_history(
  csrf_token: &str,
  contact: &history::ContactHistory,
  phone_message_capture_enabled: bool,
) -> String {
  let takeover = contact
    .takeover_until
    .as_ref()
    .map(|until| {
      format!(
        r#"<section class="dashboard-section" aria-labelledby="takeover-title">
<h2 id="takeover-title">Atención directa</h2>
<p>El asistente permanece silenciado para este contacto hasta: {}</p>
<form method="post" action="/contacts/{}/takeover/end">
<input type="hidden" name="csrf_token" value="{}">
<button class="primary-button" type="submit">Terminar atención directa</button>
</form>
</section>"#,
        escape(until),
        contact.id,
        escape(csrf_token)
      )
    })
    .unwrap_or_default();
  let conflict = if contact.identity_conflict {
    "<p class=\"form-error\" role=\"alert\">La identidad de este contacto está en conflicto. No respondas hasta revisar la asociación.</p>"
  } else {
    ""
  };
  let followup = contact
    .open_followup
    .map(|id| {
      format!(
        r#"<section class="dashboard-section" aria-labelledby="followup-title">
<h2 id="followup-title">Solicitud de atención personal</h2>
<p>El contacto pidió hablar con el doctor.</p>
<form method="post" action="/followups/{id}/resolve" aria-describedby="followup-resolution-help">
<input type="hidden" name="csrf_token" value="{}">
<button class="primary-button" type="submit">Marcar como atendida</button>
</form>
<p class="muted" id="followup-resolution-help">Esta acción cierra la solicitud y deja un aviso en cola; no confirma que hayas hablado con el contacto ni que el aviso se haya entregado.</p>
</section>"#,
        escape(csrf_token)
      )
    })
    .unwrap_or_default();
  let phone_capture_notice = if contact.identity_conflict {
    "<p class=\"muted\">Los mensajes salientes del teléfono del consultorio no se muestran en esta conversación mientras la identidad esté en conflicto, para evitar atribuirlos al contacto equivocado.</p>"
  } else if phone_message_capture_enabled {
    "<p class=\"muted\">Los mensajes de texto enviados desde el teléfono del consultorio solo aparecen cuando su atribución local a este contacto es segura; no incluyen estado de entrega o lectura.</p>"
  } else {
    "<p class=\"muted\">La captura de mensajes de texto enviados directamente desde el teléfono del consultorio está deshabilitada; ese contenido no está disponible en este historial mientras siga apagada.</p>"
  };
  let mut messages = String::new();
  let has_visible_messages = contact
    .messages
    .iter()
    .any(|message| !contact.identity_conflict || !message.human_outgoing);
  if !has_visible_messages {
    if contact.identity_conflict && !contact.messages.is_empty() {
      messages.push_str("<p class=\"empty-title\">No hay mensajes disponibles para mostrar mientras la identidad está en conflicto.</p>");
    } else {
      messages.push_str("<p class=\"empty-title\">No hay mensajes en esta conversación.</p>");
    }
  } else {
    messages.push_str("<ol class=\"conversation-list\" role=\"list\">");
    for message in &contact.messages {
      if contact.identity_conflict && message.human_outgoing {
        continue;
      }
      let direction = if message.incoming {
        "Mensaje recibido"
      } else if message.human_outgoing {
        "Mensaje saliente del teléfono del consultorio"
      } else {
        "Mensaje saliente de la aplicación"
      };
      let details = message_body(message);
      let status = if message.incoming {
        String::new()
      } else {
        delivery_status(message)
      };
      let received_at = message
        .received_at
        .as_ref()
        .map(|time| {
          let label = if message.human_outgoing {
            "Registrado por el sistema"
          } else {
            "Recibido por el sistema"
          };
          format!("<p class=\"message-time\">{label}: {}</p>", escape(time))
        })
        .unwrap_or_default();
      let dispatch_at = message
        .dispatch_at
        .as_ref()
        .map(|time| {
          format!(
            "<p class=\"message-time\">Intento de envío registrado: {}</p>",
            escape(time)
          )
        })
        .unwrap_or_default();
      let ack_at = message
        .ack_at
        .as_ref()
        .map(|time| {
          format!(
            "<p class=\"message-time\">Estado reportado por WhatsApp: {}</p>",
            escape(time)
          )
        })
        .unwrap_or_default();
      let associations = message
        .appointment_ids
        .iter()
        .map(|id| {
          format!(
            r#"<a class="text-link message-appointment" href="/appointments/{id}">Ver cita relacionada</a>"#
          )
        })
        .collect::<String>();
      let timestamp_label = if message.incoming {
        "Hora del mensaje"
      } else if message.human_outgoing {
        "Hora registrada"
      } else {
        "Registrado en la cola de la aplicación"
      };
      let message_class = if message.incoming {
        "message-incoming"
      } else if message.human_outgoing {
        "message-phone-outgoing"
      } else {
        "message-outgoing"
      };
      messages.push_str(&format!(
        r#"<li class="conversation-message {message_class}">
<h3>{direction}</h3>
<p class="message-time">{timestamp_label}: {}</p>
{received_at}{dispatch_at}{ack_at}{status}{details}{associations}
</li>"#,
        escape(&message.occurred_at)
      ));
    }
    messages.push_str("</ol>");
  }
  let pagination = timeline_links(contact);
  let label = escape(&contact.label);
  document(
    "Conversación",
    &format!(
      r#"{}
<main id="contenido" class="page-shell" tabindex="-1">
<h1>Conversación</h1>
<p class="intro">{label}</p>
<p><a class="text-link" href="/contacts">Volver a conversaciones</a></p>
{conflict}{takeover}{followup}
<section class="dashboard-section" aria-labelledby="timeline-title">
<h2 id="timeline-title">Historial de mensajes</h2>
<p class="muted">Los estados de envío describen la información disponible; no confirman asistencia a una cita.</p>
{phone_capture_notice}{messages}
{pagination}
</section>
</main>"#,
      navigation_page(csrf_token, NavigationPage::Contacts)
    ),
  )
}

pub(super) fn contact_history_error(csrf_token: &str, id: Uuid, page: u32) -> String {
  document(
    "Conversación no disponible",
    &format!(
      r#"{}
<main id="contenido" class="page-shell" tabindex="-1">
<h1>Conversación no disponible</h1>
<div class="form-error" role="alert" tabindex="-1" autofocus>
<p>No se pudo cargar la conversación. No podemos confirmar si el historial está completo.</p>
<a href="/contacts/{id}?page={page}">Volver a cargar la conversación</a>
</div>
<a class="text-link" href="/contacts">Volver a conversaciones</a>
</main>"#,
      navigation_page(csrf_token, NavigationPage::Contacts)
    ),
  )
}

fn message_body(message: &history::Message) -> String {
  let mut content = String::from("<div class=\"message-content\">");
  if let Some(deleted_at) = &message.text_deleted_at {
    content.push_str(&format!(
      "<p>El texto se eliminó conforme al plazo de conservación el {}.</p>",
      escape(deleted_at)
    ));
  } else if matches!(message.kind.as_deref(), Some("text") | Some("media")) {
    if let Some(text) = message.text.as_deref().filter(|text| !text.is_empty()) {
      content.push_str(&format!("<p>{}</p>", escape(text)));
    } else if message.kind.as_deref() == Some("text") {
      content.push_str("<p>El texto no está disponible en el registro.</p>");
    }
  }
  match message.kind.as_deref() {
    Some("media") if message.receipt_id.is_some() => {
      content.push_str("<p>Se recibió un comprobante.</p>");
    }
    Some("media") => {
      content.push_str(
        "<p>Se recibió contenido multimedia, pero este archivo no está disponible en el historial.</p>",
      );
    }
    Some("other") => {
      content.push_str("<p>El contenido recibido no se puede mostrar en este historial.</p>");
    }
    Some("text") => {}
    _ if message.text_deleted_at.is_none() => {
      content.push_str("<p>El mensaje todavía no está listo para mostrarse en el historial.</p>");
    }
    _ => {}
  }
  if let Some(receipt_id) = message.receipt_id {
    if message.receipt_deleted {
      content.push_str("<p>El archivo del comprobante no está disponible.</p>");
    } else {
      content.push_str(&format!(
        r#"<a class="text-link receipt-link" href="/dashboard/payments/receipts/{receipt_id}">Revisar comprobante</a>"#
      ));
    }
  }
  if !message.incoming && message.ack_error {
    content.push_str(
      "<p class=\"muted\">WhatsApp también reportó un error; esto no descarta una entrega o lectura anterior.</p>",
    );
  }
  content.push_str("</div>");
  content
}

fn delivery_status(message: &history::Message) -> String {
  if message.human_outgoing {
    return String::from(
      "<p class=\"message-status\">Estado de entrega o lectura: no disponible para mensajes enviados directamente desde el teléfono.</p>",
    );
  }
  let status = match message.positive_ack {
    4 => "Reproducción o visualización reportada por WhatsApp",
    3 => "Leído según WhatsApp",
    2 => "Entregado al dispositivo según WhatsApp",
    1 => "Enviado a WhatsApp",
    _ => match message.outbox_state.as_deref() {
      Some("queued") => "En cola",
      Some("typing") => "En preparación para enviar",
      Some("sent") => "Enviado a WhatsApp",
      Some("uncertain") => "Envío incierto; el resultado no se pudo confirmar",
      Some("failed") => "Envío fallido",
      Some("cancelled") => "Cancelado antes de confirmar el envío",
      _ => "Estado de envío no disponible",
    },
  };
  format!("<p class=\"message-status\">Estado: {status}</p>")
}

fn page_links(base: &str, page: u32, previous_page: Option<u32>, next_page: bool) -> String {
  if previous_page.is_none() && !next_page {
    return String::new();
  }
  let mut links =
    String::from("<nav class=\"attention-pagination\" aria-label=\"Páginas de conversaciones\">");
  if let Some(previous) = previous_page {
    links.push_str(&format!(
      r#"<a class="text-link" href="{base}?page={previous}" rel="prev">Página anterior</a>"#
    ));
  }
  if next_page {
    links.push_str(&format!(
      r#"<a class="text-link" href="{base}?page={}" rel="next">Página siguiente</a>"#,
      page.saturating_add(1)
    ));
  }
  links.push_str("</nav>");
  links
}

fn timeline_links(contact: &history::ContactHistory) -> String {
  if contact.previous_page.is_none() && contact.next_page.is_none() {
    return String::new();
  }
  let mut links =
    String::from("<nav class=\"attention-pagination\" aria-label=\"Páginas de mensajes\">");
  if let Some(page) = contact.previous_page {
    links.push_str(&format!(
      r#"<a class="text-link" href="/contacts/{}?page={page}" rel="prev">Mensajes más recientes</a>"#,
      contact.id
    ));
  }
  if let Some(page) = contact.next_page {
    links.push_str(&format!(
      r#"<a class="text-link" href="/contacts/{}?page={page}" rel="next">Mensajes anteriores</a>"#,
      contact.id
    ));
  }
  links.push_str("</nav>");
  links
}

pub fn privacy() -> String {
  document(
    "Aviso de privacidad provisional",
    r#"<main id="contenido" class="page-shell privacy-notice">
<h1>Aviso de privacidad provisional</h1>
<p class="intro"><strong>PROVISIONAL — no aprobado legalmente.</strong> Este texto describe el uso administrativo actual del asistente. No es un aviso definitivo ni acredita cumplimiento legal.</p>
<section class="dashboard-section" aria-labelledby="purpose-title">
<h2 id="purpose-title">Uso administrativo actual</h2>
<p class="muted">Si usted escribe al WhatsApp del consultorio, el asistente utiliza el identificador de su chat, su número cuando está disponible, sus mensajes y los registros de envío para gestionar citas, comprobantes, recordatorios y solicitudes de atención personal. El doctor consulta esta información en su panel protegido. El nombre de la persona que asistirá se registra en esa cita, no en un expediente de pacientes.</p>
<p class="muted">Las solicitudes, cambios y cancelaciones de citas, así como los comprobantes, son trámites administrativos del consultorio; no son atención médica ni forman un expediente clínico. No envíe síntomas, diagnósticos, estudios, recetas ni otros datos clínicos.</p>
</section>
<section class="dashboard-section" aria-labelledby="minimal-data-title">
<h2 id="minimal-data-title">Comparta solo lo necesario</h2>
<p class="muted">Por favor, no envíe síntomas, diagnósticos, estudios, recetas ni otros datos clínicos. Este asistente no es un expediente clínico ni un servicio de atención médica. Para solicitar atención personal, elija la opción 5 del menú, «Hablar con el doctor», sin incluir información clínica.</p>
</section>
<section class="dashboard-section" aria-labelledby="pending-details-title">
<h2 id="pending-details-title">Información pendiente de completar</h2>
<p class="muted">Están pendientes la identidad y el domicilio del responsable del tratamiento, los datos de contacto para privacidad y el procedimiento para ejercer sus derechos de acceso, rectificación, cancelación y oposición (ARCO).</p>
<p class="muted">Estos datos y las demás condiciones del aviso deben completarse y revisarse legalmente antes de su publicación definitiva. Este texto no solicita ni acredita su consentimiento para el tratamiento de datos.</p>
</section>
</main>"#,
  )
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn contact_history_escapes_content_and_distinguishes_phone_source() {
    let message = |kind: &str, text: &str| history::Message {
      incoming: true,
      human_outgoing: false,
      occurred_at: "2026-10-16 12:00 UTC".into(),
      received_at: None,
      dispatch_at: None,
      ack_at: None,
      text: Some(text.into()),
      kind: Some(kind.into()),
      text_deleted_at: None,
      outbox_state: None,
      positive_ack: 0,
      ack_error: false,
      appointment_ids: vec![],
      receipt_id: None,
      receipt_deleted: false,
    };
    let mut phone_message = message("text", "Mensaje de prueba del teléfono");
    phone_message.incoming = false;
    phone_message.human_outgoing = true;
    let contact_id = Uuid::now_v7();
    let contact = history::ContactHistory {
      id: contact_id,
      label: "Contacto".into(),
      identity_conflict: false,
      takeover_until: None,
      open_followup: None,
      messages: vec![
        message("text", "<script>alert(1)</script>&"),
        message("other", "UNSUPPORTED-PRIVATE-PAYLOAD"),
        phone_message,
      ],
      previous_page: Some(1),
      next_page: Some(3),
    };

    let html = contact_history("csrf", &contact, true);

    assert!(html.contains("&lt;script&gt;alert(1)&lt;/script&gt;&amp;"));
    assert!(!html.contains("<script>"));
    assert!(!html.contains("UNSUPPORTED-PRIVATE-PAYLOAD"));
    assert!(html.contains("Mensaje saliente del teléfono del consultorio"));
    assert!(html.contains("Estado de entrega o lectura: no disponible"));
    assert!(!html.contains("Estado: Enviado a WhatsApp"));
    assert!(!html.contains("Leído según WhatsApp"));
    assert!(html.contains(&format!("/contacts/{contact_id}?page=1")));
    assert!(html.contains(&format!("/contacts/{contact_id}?page=3")));
    let mut conflict_contact = contact;
    conflict_contact.identity_conflict = true;
    let conflict_html = contact_history("csrf", &conflict_contact, true);
    assert!(!conflict_html.contains("Mensaje saliente del teléfono del consultorio"));
    assert!(!conflict_html.contains("Mensaje de prueba del teléfono"));
    assert!(conflict_html.contains("mientras la identidad esté en conflicto"));
  }
}
