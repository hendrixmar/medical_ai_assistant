use crate::modules::doctor::views::{document, escape, navigation_page, NavigationPage};

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Period {
  Upcoming,
  Past,
}

impl Period {
  pub(super) fn query(self) -> &'static str {
    match self {
      Self::Upcoming => "upcoming",
      Self::Past => "past",
    }
  }
}

pub(super) struct ListItem {
  pub id: String,
  pub patient_name: String,
  pub service_name: String,
  pub format: String,
  pub starts_at: String,
  pub status: String,
  pub payment: String,
  pub calendar_sync: String,
}

pub(super) struct ListView<'a> {
  pub csrf_token: &'a str,
  pub period: Period,
  pub page: u32,
  pub items: &'a [ListItem],
  pub previous_page: Option<u32>,
  pub next_page: Option<u32>,
  pub load_error: bool,
}

pub(super) fn appointment_list(view: ListView<'_>) -> String {
  let content = if view.load_error {
    format!(
      r#"<div class="form-error" role="alert" tabindex="-1" autofocus>
<p>No se pudieron cargar las citas. No podemos confirmar si hay más resultados.</p>
<a href="/appointments?period={}&page={}">Volver a cargar las citas</a>
</div>"#,
      view.period.query(),
      view.page
    )
  } else if view.items.is_empty() {
    let message = if view.previous_page.is_some() {
      "No hay citas en esta página."
    } else {
      match view.period {
        Period::Upcoming => "No hay citas próximas en este momento.",
        Period::Past => "No hay citas pasadas, canceladas o vencidas para mostrar.",
      }
    };
    format!("<p class=\"empty-title\">{message}</p>")
  } else {
    let mut content = String::from("<ul class=\"appointment-list\" role=\"list\">");
    for item in view.items {
      content.push_str(&format!(
        r#"<li>
<h2><a href="/appointments/{}">Cita de {}</a></h2>
<p>{} · {}</p>
<p class="appointment-time">{}</p>
<p class="muted">Estado: {}. Pago: {}. Calendario: {}.</p>
</li>"#,
        escape(&item.id),
        escape(&item.patient_name),
        escape(&item.service_name),
        escape(&item.format),
        escape(&item.starts_at),
        escape(&item.status),
        escape(&item.payment),
        escape(&item.calendar_sync)
      ));
    }
    content.push_str("</ul>");
    content
  };
  let tabs = format!(
    r#"<nav class="appointment-period" aria-label="Filtrar citas">
<a href="/appointments?period=upcoming"{}>Próximas</a>
<a href="/appointments?period=past"{}>Pasadas</a>
</nav>"#,
    if view.period == Period::Upcoming {
      " aria-current=\"page\""
    } else {
      ""
    },
    if view.period == Period::Past {
      " aria-current=\"page\""
    } else {
      ""
    }
  );
  let pagination = list_pagination(&view);
  document(
    "Citas",
    &format!(
      r#"{}
<main id="contenido" class="page-shell" tabindex="-1">
<h1>Citas</h1>
<p class="intro">Consulta las citas registradas; una cita pasada no se marca como atendida ni como inasistencia.</p>
{tabs}
{content}
{pagination}
</main>"#,
      navigation_page(view.csrf_token, NavigationPage::Appointments)
    ),
  )
}

fn list_pagination(view: &ListView<'_>) -> String {
  if view.previous_page.is_none() && view.next_page.is_none() {
    return String::new();
  }
  let mut links =
    String::from("<nav class=\"attention-pagination\" aria-label=\"Páginas de citas\">");
  if let Some(page) = view.previous_page {
    links.push_str(&format!(
      r#"<a class="text-link" href="/appointments?period={}&page={page}" rel="prev">Página anterior</a>"#,
      view.period.query()
    ));
  }
  if let Some(page) = view.next_page {
    links.push_str(&format!(
      r#"<a class="text-link" href="/appointments?period={}&page={page}" rel="next">Página siguiente</a>"#,
      view.period.query()
    ));
  }
  links.push_str("</nav>");
  links
}

pub(super) struct ReceiptLink {
  pub id: String,
  pub status: String,
  pub sent_at: String,
  pub arrived_at: String,
  pub file_available: bool,
}

pub(super) struct HistoryEntry {
  pub actor: String,
  pub action: String,
  pub occurred_at: String,
}

pub(super) struct DetailView {
  pub id: String,
  pub contact_id: String,
  pub patient_name: String,
  pub service_name: String,
  pub format: String,
  pub starts_at: String,
  pub ends_at: String,
  pub duration: String,
  pub price: String,
  pub location: Option<String>,
  pub status: String,
  pub payment: String,
  pub calendar_sync: String,
  pub receipt_deadline: Option<String>,
  pub meet_url: Option<String>,
  pub attendance_confirmed_at: Option<String>,
  pub can_cancel: bool,
  pub can_reschedule: bool,
  pub can_resolve_refund: bool,
  pub receipts: Vec<ReceiptLink>,
  pub history: Vec<HistoryEntry>,
}

pub(super) fn appointment_detail(csrf_token: &str, view: &DetailView) -> String {
  let location = view
    .location
    .as_deref()
    .map(|location| format!("<div><dt>Ubicación</dt><dd>{}</dd></div>", escape(location)))
    .unwrap_or_default();
  let deadline = view
    .receipt_deadline
    .as_deref()
    .map(|time| {
      format!(
        "<div><dt>Fecha límite del comprobante</dt><dd>{}</dd></div>",
        escape(time)
      )
    })
    .unwrap_or_default();
  let meet = view
    .meet_url
    .as_deref()
    .map(|url| {
      format!(
        "<div><dt>Enlace de Meet</dt><dd class=\"appointment-url\">{}</dd></div>",
        escape(url)
      )
    })
    .unwrap_or_default();
  let attendance = view
    .attendance_confirmed_at
    .as_deref()
    .map(|time| {
      format!(
        "<p class=\"muted\">El contacto confirmó asistencia el {}.</p>",
        escape(time)
      )
    })
    .unwrap_or_default();
  let mut actions = String::new();
  if view.can_reschedule {
    actions.push_str(&format!(
      r#"<form method="post" action="/appointments/{}/reschedule">
<input type="hidden" name="csrf_token" value="{}">
<button class="primary-button" type="submit">Pedir reprogramación</button>
</form>"#,
      escape(&view.id),
      escape(csrf_token)
    ));
  }
  if view.can_cancel {
    actions.push_str(&format!(
      r#"<form method="post" action="/appointments/{}/cancel">
<input type="hidden" name="csrf_token" value="{}">
<button class="primary-button" type="submit">Cancelar cita</button>
</form>"#,
      escape(&view.id),
      escape(csrf_token)
    ));
  }
  if view.can_resolve_refund {
    actions.push_str(&format!(
      r#"<form method="post" action="/appointments/{}/refund/resolve" aria-describedby="refund-help">
<input type="hidden" name="csrf_token" value="{}">
<p id="refund-help">El reembolso se gestiona fuera del asistente. Registra esta resolución sólo después de atenderlo; esta acción no transfiere fondos ni cambia el pago verificado.</p>
<button class="primary-button" type="submit">Marcar reembolso como resuelto</button>
</form>"#,
      escape(&view.id),
      escape(csrf_token)
    ));
  }
  let actions = if actions.is_empty() {
    String::new()
  } else {
    format!("<div class=\"appointment-actions\">{actions}</div>")
  };
  let receipts = if view.receipts.is_empty() {
    "<p class=\"muted\">No hay comprobantes vinculados a esta cita.</p>".to_owned()
  } else {
    let mut content = String::from("<ul class=\"appointment-history\" role=\"list\">");
    for receipt in &view.receipts {
      let file_status = if receipt.file_available {
        "El archivo protegido está disponible para revisión."
      } else {
        "El archivo del comprobante no está disponible."
      };
      let link_label = if receipt.file_available {
        "Revisar comprobante"
      } else {
        "Ver estado del comprobante"
      };
      content.push_str(&format!(
        r#"<li>
<h3>{}</h3>
<p>Enviado según WhatsApp: {}</p>
<p class="appointment-time">Recibido: {}</p>
<p>{file_status}</p>
<a class="text-link" href="/dashboard/payments/receipts/{}">{link_label}</a>
</li>"#,
        escape(&receipt.status),
        escape(&receipt.sent_at),
        escape(&receipt.arrived_at),
        escape(&receipt.id)
      ));
    }
    content.push_str("</ul>");
    content
  };
  let history = if view.history.is_empty() {
    "<p class=\"empty-title\">No hay cambios registrados en el historial.</p>".to_owned()
  } else {
    let mut content = String::from("<ol class=\"appointment-history\" role=\"list\">");
    for entry in &view.history {
      content.push_str(&format!(
        r#"<li>
<h3>{}</h3>
<p>Actor: {}</p>
<p class="appointment-time">{}</p>
</li>"#,
        escape(&entry.action),
        escape(&entry.actor),
        escape(&entry.occurred_at)
      ));
    }
    content.push_str("</ol>");
    content
  };
  document(
    "Detalle de la cita",
    &format!(
      r#"{}
<main id="contenido" class="page-shell" tabindex="-1">
<h1>Detalle de la cita</h1>
<p class="intro">Cita de {}</p>
<p><a class="text-link" href="/appointments">Volver a las citas</a></p>
<section class="dashboard-section" aria-labelledby="appointment-details-title">
<h2 id="appointment-details-title">Datos de la cita</h2>
<dl class="appointment-facts">
<div><dt>Paciente</dt><dd>{}</dd></div>
<div><dt>Servicio</dt><dd>{}</dd></div>
<div><dt>Formato</dt><dd>{}</dd></div>
<div><dt>Inicio</dt><dd>{}</dd></div>
<div><dt>Fin</dt><dd>{}</dd></div>
<div><dt>Duración</dt><dd>{}</dd></div>
<div><dt>Precio acordado</dt><dd>{}</dd></div>
{location}
{deadline}
{meet}
<div><dt>Estado</dt><dd>{}</dd></div>
<div><dt>Pago</dt><dd>{}</dd></div>
<div><dt>Calendario</dt><dd>{}</dd></div>
</dl>
<p><a class="text-link" href="/contacts/{}">Ver conversación del contacto</a></p>
{attendance}
{actions}
</section>
<section class="dashboard-section" aria-labelledby="receipts-title">
<h2 id="receipts-title">Comprobantes</h2>
<p class="muted">Un comprobante es evidencia de una transferencia, no confirmación de que se recibió el pago.</p>
{receipts}
</section>
<section class="dashboard-section" aria-labelledby="appointment-history-title">
<h2 id="appointment-history-title">Historial de la cita</h2>
{history}
</section>
</main>"#,
      navigation_page(csrf_token, NavigationPage::Appointments),
      escape(&view.patient_name),
      escape(&view.patient_name),
      escape(&view.service_name),
      escape(&view.format),
      escape(&view.starts_at),
      escape(&view.ends_at),
      escape(&view.duration),
      escape(&view.price),
      escape(&view.status),
      escape(&view.payment),
      escape(&view.calendar_sync),
      escape(&view.contact_id),
      location = location,
      deadline = deadline,
      meet = meet,
      attendance = attendance,
      actions = actions,
      receipts = receipts,
      history = history,
    ),
  )
}

pub(super) fn load_error(csrf_token: &str, id: Option<&str>) -> String {
  let retry = id
    .map(|id| {
      format!(
        "<a href=\"/appointments/{}\">Volver a cargar la cita</a>",
        escape(id)
      )
    })
    .unwrap_or_else(|| "<a href=\"/appointments\">Volver a cargar las citas</a>".to_owned());
  document(
    "Citas no disponibles",
    &format!(
      r#"{}
<main id="contenido" class="page-shell" tabindex="-1">
<h1>Citas no disponibles</h1>
<div class="form-error" role="alert" tabindex="-1" autofocus>
<p>No se pudieron cargar los datos. No podemos confirmar si hay citas o cambios registrados.</p>
{retry}
</div>
</main>"#,
      navigation_page(csrf_token, NavigationPage::Appointments)
    ),
  )
}
