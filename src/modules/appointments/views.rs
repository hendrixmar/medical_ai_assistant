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
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum CalendarMode {
  Day,
  Week,
  Month,
}

impl CalendarMode {
  pub(super) fn query(self) -> &'static str {
    match self {
      Self::Day => "day",
      Self::Week => "week",
      Self::Month => "month",
    }
  }

  pub(super) fn label(self) -> &'static str {
    match self {
      Self::Day => "Día",
      Self::Week => "Semana",
      Self::Month => "Mes",
    }
  }
}

pub(super) struct CalendarSlotItem {
  pub id: String,
  pub patient_name: String,
  pub service_name: String,
  pub format: String,
  pub time_range: String,
  pub status: String,
  pub hold_active: bool,
}

pub(super) struct CalendarDayGroup {
  pub date_label: String,
  pub items: Vec<CalendarSlotItem>,
}

pub(super) struct ServiceOption {
  pub id: String,
  pub label: String,
}

pub(super) struct ContactOption {
  pub id: String,
  pub label: String,
}

pub(super) struct CreateFormView<'a> {
  pub services: &'a [ServiceOption],
  pub contacts: &'a [ContactOption],
  pub timezone: Option<&'a str>,
  pub error: Option<&'a str>,
}

pub(super) struct CalendarView<'a> {
  pub csrf_token: &'a str,
  pub mode: CalendarMode,
  pub anchor_date: &'a str,
  pub prev_date: &'a str,
  pub next_date: &'a str,
  pub range_label: &'a str,
  pub timezone: &'a str,
  pub days: &'a [CalendarDayGroup],
  pub create_form: CreateFormView<'a>,
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
  pub create_form: Option<CreateFormView<'a>>,
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
<a href="/appointments?view=day">Día</a>
<a href="/appointments?view=week">Semana</a>
<a href="/appointments?view=month">Mes</a>
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
  let create_section = view
    .create_form
    .as_ref()
    .map(|form| render_create_form(view.csrf_token, form))
    .unwrap_or_default();
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
{create_section}
</main>"#,
      navigation_page(view.csrf_token, NavigationPage::Appointments)
    ),
  )
}

pub(super) fn appointment_calendar(view: CalendarView<'_>) -> String {
  let tabs = format!(
    r#"<nav class="appointment-period" aria-label="Cambiar vista de agenda">
<a href="/appointments?period=upcoming">Próximas</a>
<a href="/appointments?period=past">Pasadas</a>
<a href="/appointments?view=day&amp;date={date}"{day_current}>Día</a>
<a href="/appointments?view=week&amp;date={date}"{week_current}>Semana</a>
<a href="/appointments?view=month&amp;date={date}"{month_current}>Mes</a>
</nav>"#,
    date = escape(view.anchor_date),
    day_current = if view.mode == CalendarMode::Day {
      " aria-current=\"page\""
    } else {
      ""
    },
    week_current = if view.mode == CalendarMode::Week {
      " aria-current=\"page\""
    } else {
      ""
    },
    month_current = if view.mode == CalendarMode::Month {
      " aria-current=\"page\""
    } else {
      ""
    },
  );
  let mode_query = view.mode.query();
  let nav = format!(
    r#"<nav class="attention-pagination" aria-label="Navegar fechas de calendario">
<a class="text-link" href="/appointments?view={mode_query}&amp;date={}" rel="prev">Periodo anterior</a>
<span><strong>{} ({})</strong></span>
<a class="text-link" href="/appointments?view={mode_query}&amp;date={}" rel="next">Periodo siguiente</a>
</nav>"#,
    escape(view.prev_date),
    escape(view.range_label),
    escape(view.timezone),
    escape(view.next_date),
  );
  let mut grid =
    String::from("<section class=\"calendar-grid\" aria-label=\"Calendario de citas\">");
  for day in view.days {
    grid.push_str(&format!(
      "<article class=\"dashboard-section\"><h2>{}</h2>",
      escape(&day.date_label)
    ));
    if day.items.is_empty() {
      grid.push_str("<p class=\"muted\">Sin citas ni retenciones activas.</p>");
    } else {
      grid.push_str("<ul class=\"appointment-list\" role=\"list\">");
      for item in &day.items {
        let hold_badge = if item.hold_active {
          " · Retención activa"
        } else {
          ""
        };
        grid.push_str(&format!(
          r#"<li><h3><a href="/appointments/{}">{} — {}</a></h3><p>{} · {}{}</p><p class="muted">Estado: {}</p></li>"#,
          escape(&item.id),
          escape(&item.time_range),
          escape(&item.patient_name),
          escape(&item.service_name),
          escape(&item.format),
          hold_badge,
          escape(&item.status),
        ));
      }
      grid.push_str("</ul>");
    }
    grid.push_str("</article>");
  }
  grid.push_str("</section>");
  let create_section = render_create_form(view.csrf_token, &view.create_form);
  document(
    &format!("Calendario ({})", view.mode.label()),
    &format!(
      r#"{}
<main id="contenido" class="page-shell" tabindex="-1">
<h1>Calendario de citas — {}</h1>
<p class="intro">Muestra las mismas citas y retenciones activas que consulta el asistente de WhatsApp.</p>
{tabs}
{nav}
{grid}
{create_section}
</main>"#,
      navigation_page(view.csrf_token, NavigationPage::Appointments),
      escape(view.mode.label()),
    ),
  )
}

fn render_create_form(csrf_token: &str, form: &CreateFormView<'_>) -> String {
  let error_banner = form
    .error
    .map(|msg| {
      format!(
        r#"<div class="form-error" role="alert" tabindex="-1" autofocus><p>{}</p></div>"#,
        escape(msg)
      )
    })
    .unwrap_or_default();
  let Some(tz) = form.timezone else {
    return format!(
      r#"<section class="dashboard-section" aria-labelledby="manual-create-title">
<h2 id="manual-create-title">Registrar cita desde el panel</h2>
{error_banner}
<p class="muted">Completa primero los datos, servicios y horarios del consultorio en Configuración antes de registrar citas manualmente.</p>
</section>"#
    );
  };
  if form.services.is_empty() {
    return format!(
      r#"<section class="dashboard-section" aria-labelledby="manual-create-title">
<h2 id="manual-create-title">Registrar cita desde el panel</h2>
{error_banner}
<p class="muted">Añade al menos un servicio en Configuración para poder registrar citas.</p>
</section>"#
    );
  }
  let mut service_options = String::new();
  for svc in form.services {
    service_options.push_str(&format!(
      "<option value=\"{}\">{}</option>",
      escape(&svc.id),
      escape(&svc.label)
    ));
  }
  let mut contact_options =
    String::from("<option value=\"\">Nuevo contacto o teléfono indicado abajo</option>");
  for contact in form.contacts {
    contact_options.push_str(&format!(
      "<option value=\"{}\">{}</option>",
      escape(&contact.id),
      escape(&contact.label)
    ));
  }
  format!(
    r#"<section class="dashboard-section" aria-labelledby="manual-create-title">
<h2 id="manual-create-title">Registrar cita desde el panel</h2>
<p class="muted">Aplica las mismas reglas de horario, retenciones (Holds), sincronización con Google Calendar y recordatorios ({tz}).</p>
{error_banner}
<form class="settings-form" method="post" action="/appointments/create">
<input type="hidden" name="csrf_token" value="{csrf}">
<div class="field"><label for="patient_name">Nombre del paciente (solo para esta cita)</label><input id="patient_name" name="patient_name" type="text" maxlength="120" required></div>
<div class="field"><label for="contact_id">Contacto existente (opcional)</label><select id="contact_id" name="contact_id">{contact_options}</select></div>
<div class="field"><label for="contact_phone">Teléfono de WhatsApp (si no eliges un contacto existente)</label><input id="contact_phone" name="contact_phone" type="text" placeholder="5215512345678"></div>
<div class="field"><label for="service_id">Servicio</label><select id="service_id" name="service_id" required>{service_options}</select></div>
<div class="field"><label for="format">Formato</label><select id="format" name="format" required><option value="in_person">Presencial</option><option value="video">Videollamada</option></select></div>
<div class="field-pair">
<div class="field"><label for="date">Fecha ({tz})</label><input id="date" name="date" type="date" required></div>
<div class="field"><label for="time">Hora ({tz})</label><input id="time" name="time" type="time" required></div>
</div>
<div class="form-actions"><button class="primary-button" type="submit">Registrar cita</button></div>
</form>
</section>"#,
    tz = escape(tz),
    csrf = escape(csrf_token),
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
