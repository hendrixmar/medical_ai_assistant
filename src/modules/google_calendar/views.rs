use crate::modules::doctor::views::{document, escape, navigation};

pub enum ConnectionView {
  Unconfigured,
  Disconnected,
  Connected,
  ReauthorizationRequired,
  Unavailable,
}

pub struct CalendarChoice {
  pub id: String,
  pub label: String,
  pub booking_candidate: bool,
}

pub enum PermissionView {
  Unverified,
  Verified { checked_at: String },
}

pub enum CheckView {
  None,
  Queued,
  Running,
  Passed { checked_at: String },
  Failed { reason: String },
  CleanupPending { reason: String },
}

pub struct CleanupItem {
  pub id: String,
  pub calendar_label: String,
  pub occurred_at: String,
  pub reason: String,
  pub can_retry: bool,
}

pub struct GoogleView<'a> {
  pub csrf_token: &'a str,
  pub connection: ConnectionView,
  pub generation: i64,
  pub selection_version: i64,
  pub current_probe_id: Option<&'a str>,
  pub calendars: &'a [CalendarChoice],
  pub selected_booking: Option<&'a str>,
  pub selected_blocking: &'a [String],
  pub saved_booking_label: Option<&'a str>,
  pub calendars_available: bool,
  pub permission: PermissionView,
  pub check: CheckView,
  pub cleanups: &'a [CleanupItem],
  pub connect_block: Option<&'a str>,
  pub selection_block: Option<&'a str>,
  pub probe_block: Option<&'a str>,
  pub notice: Option<&'a str>,
  pub load_error: bool,
  pub cleanup_previous_page: Option<u32>,
  pub cleanup_next_page: Option<u32>,
}

pub fn google(view: GoogleView<'_>) -> String {
  let content = if view.load_error {
    String::from(
      "<div class=\"form-error\" role=\"alert\" tabindex=\"-1\" autofocus><h2>No se pudo cargar la configuración de Google</h2><p>No podemos confirmar la conexión, la selección ni los eventos de prueba pendientes de limpieza. No se muestran datos anteriores ni acciones. Actualiza el estado para volver a consultar.</p></div>",
    )
  } else {
    let notice = view
      .notice
      .map(|text| {
        format!(
          "<div class=\"form-error\" role=\"alert\" tabindex=\"-1\" autofocus>{}</div>",
          escape(text)
        )
      })
      .unwrap_or_default();
    let cleanup_warning = if !view.cleanups.is_empty() {
      "<p class=\"empty-title\"><strong>Hay eventos de prueba cuya limpieza sigue pendiente.</strong></p><a class=\"text-link\" href=\"#google-cleanup\">Revisar limpieza pendiente</a>"
    } else {
      ""
    };
    format!(
      "{notice}{cleanup_warning}{}{}{}{}",
      connection(&view),
      calendars(&view),
      check(&view),
      cleanups(&view)
    )
  };
  document(
    "Conexión de Google Calendar",
    &format!(
      r#"{}
<main id="contenido" class="page-shell google-settings" tabindex="-1">
<h1>Conexión de Google Calendar</h1>
<p class="intro">Conecta Google, elige tus calendarios y comprueba los permisos y Google Meet. Las citas y los pagos se gestionan desde el panel; conectar Google no verifica pagos ni confirma por sí solo una cita.</p>
{content}
<div class="doctor-number-links">
<a class="text-link" href="/settings/google">Actualizar estado</a>
<a class="text-link" href="/settings">Volver a configuración</a>
</div>
<p class="muted">Esta página no se actualiza sola. Actualizar el estado no inicia una prueba nueva.</p>
</main>"#,
      navigation(view.csrf_token, true)
    ),
  )
}

fn connection(view: &GoogleView<'_>) -> String {
  let (title, description) = match view.connection {
    ConnectionView::Unconfigured => (
      "Google no está configurado",
      "Pide a quien administra esta instalación que complete la configuración de Google. No puedes conectar una cuenta todavía.",
    ),
    ConnectionView::Disconnected => (
      "Sin conexión de Google",
      "Conecta una cuenta para consultar los calendarios disponibles y elegir cuáles usar.",
    ),
    ConnectionView::Connected => (
      "Conexión de Google disponible",
      "La conexión permite consultar Google. Esto no demuestra todavía los permisos sobre eventos ni la disponibilidad de Google Meet.",
    ),
    ConnectionView::ReauthorizationRequired => (
      "Necesitas volver a conectar Google",
      "La autorización guardada no se puede usar. Vuelve a conectar para continuar; esto no significa necesariamente que hayas revocado el acceso.",
    ),
    ConnectionView::Unavailable => (
      "No se puede consultar Google ahora",
      "No podemos confirmar la conexión actual. Los datos guardados se conservan como historial; no confirman acceso actual. Puedes actualizar el estado para volver a consultar.",
    ),
  };
  let action = if let Some(reason) = view.connect_block {
    blocked("No puedes conectar ahora", reason)
  } else if matches!(view.connection, ConnectionView::Unconfigured) {
    String::new()
  } else {
    let label = if matches!(view.connection, ConnectionView::Disconnected) {
      "Conectar Google"
    } else {
      "Volver a conectar Google"
    };
    format!(
      r#"<form class="settings-form" method="post" action="/settings/google/connect" aria-describedby="google-reconnect">
{}
<div class="form-actions"><button class="primary-button" type="submit">{label}</button></div>
</form>"#,
      csrf(view)
    )
  };
  format!(
    r#"<section class="dashboard-section" aria-labelledby="google-connection">
<h2 id="google-connection">{title}</h2>
<p class="muted">{description}</p>
<p class="muted" id="google-reconnect">Al conectar puedes elegir otra cuenta. Una nueva conexión requiere volver a seleccionar los calendarios y comprobar sus permisos; no cambia los demás datos del consultorio ni borra las limpiezas pendientes.</p>
{action}
</section>"#
  )
}

fn calendars(view: &GoogleView<'_>) -> String {
  let fresh = matches!(view.connection, ConnectionView::Connected) && view.calendars_available;
  let saved = if let Some(id) = view.selected_booking {
    let label = escape(view.saved_booking_label.unwrap_or(id));
    let freshness = if fresh {
      "Selección guardada; el acceso a disponibilidad se validó al guardarla. La lista disponible se consultó ahora. Esto no demuestra permisos sobre eventos."
    } else {
      "Selección guardada (histórica). El acceso a disponibilidad se validó al guardarla, pero no podemos confirmar acceso ni permisos actuales."
    };
    format!(
      "<p class=\"empty-title\"><strong>Calendario seleccionado para citas:</strong> {label}</p><p class=\"muted\">{freshness}</p>"
    )
  } else {
    String::from(
      "<p class=\"muted\">No hay un calendario para citas seleccionado. Conectar Google no selecciona calendarios automáticamente.</p>",
    )
  };
  let mut blocking_saved = String::new();
  if !fresh && !view.selected_blocking.is_empty() {
    blocking_saved.push_str("<p class=\"muted\">Calendarios de bloqueo guardados (identificadores históricos):</p><ul class=\"google-saved-list\">");
    for id in view.selected_blocking {
      blocking_saved.push_str(&format!("<li>{}</li>", escape(id)));
    }
    blocking_saved.push_str("</ul>");
  }
  let selection = if let Some(reason) = view.selection_block {
    blocked("No puedes guardar calendarios ahora", reason)
  } else if !fresh {
    String::from(
      "<p class=\"muted\">Para cambiar la selección necesitas una conexión disponible y una lista completa recién consultada.</p>",
    )
  } else if !view.calendars.iter().any(|item| item.booking_candidate) {
    String::from(
      "<p class=\"empty-title\"><strong>No hay calendarios disponibles que puedas administrar para recibir citas.</strong></p><p class=\"muted\">Tener permiso para escribir eventos no basta. Revisa tus permisos en Google o conecta otra cuenta.</p>",
    )
  } else {
    selection_form(view)
  };
  format!(
    r#"<section class="dashboard-section" aria-labelledby="google-calendars">
<h2 id="google-calendars">Calendarios</h2>
{saved}{blocking_saved}
<p class="muted" id="google-calendar-rights">Para recibir citas, elige un calendario que puedas administrar: Google debe reportar el rol «owner», que permite administrar también el acceso compartido. Puede ser secundario o compartido; no demuestra propiedad exclusiva ni verifica la identidad de una cuenta. Los nombres son etiquetas de calendario proporcionadas por Google.</p>
{selection}
</section>"#
  )
}

fn selection_form(view: &GoogleView<'_>) -> String {
  let mut options = String::from("<option value=\"\">Elige un calendario</option>");
  let mut choices = String::new();
  for (index, item) in view.calendars.iter().enumerate() {
    let id = escape(&item.id);
    let label = escape(&item.label);
    if item.booking_candidate {
      let selected = if view.selected_booking == Some(item.id.as_str()) {
        " selected"
      } else {
        ""
      };
      options.push_str(&format!(
        "<option value=\"{id}\"{selected}>{label}</option>"
      ));
    }
    let checked = if view.selected_blocking.contains(&item.id) {
      " checked"
    } else {
      ""
    };
    choices.push_str(&format!(
      "<label class=\"google-checkbox\" for=\"blocking-{index}\"><input id=\"blocking-{index}\" type=\"checkbox\" name=\"blocking_calendar\" value=\"{id}\"{checked}><span>{label}</span></label>"
    ));
  }
  format!(
    r#"<form class="settings-form" method="post" action="/settings/google/calendars" aria-labelledby="google-calendars" aria-describedby="google-calendar-rights google-selection-reset">
{}
<div class="field">
<label for="booking-calendar">Calendario para recibir citas</label>
<p class="field-hint" id="booking-help">Solo aparecen los calendarios que puedes administrar. Sus permisos sobre eventos se comprueban por separado.</p>
<select id="booking-calendar" name="booking_calendar" aria-describedby="booking-help" required>{options}</select>
</div>
<fieldset class="google-blocking" aria-describedby="blocking-help">
<legend>Calendarios que bloquean disponibilidad</legend>
<p class="field-hint" id="blocking-help">Marca explícitamente de 0 a 100 calendarios. Puedes incluir el calendario para citas; no se marca automáticamente. Solo se consulta disponibilidad, no los detalles de sus eventos.</p>
{choices}
</fieldset>
<p class="muted" id="google-selection-reset">Guardar valida el acceso a disponibilidad y reinicia las comprobaciones de permisos y Meet para esta selección. Las limpiezas pendientes se conservan.</p>
<div class="form-actions"><button class="primary-button" type="submit">Guardar calendarios</button></div>
</form>"#,
    version_fields(view)
  )
}

fn check(view: &GoogleView<'_>) -> String {
  let connected = matches!(view.connection, ConnectionView::Connected);
  let permission = match &view.permission {
    PermissionView::Unverified => String::from(
      "Permisos sobre eventos: sin comprobar. La selección del calendario no basta para demostrarlos.",
    ),
    PermissionView::Verified { checked_at } => format!(
      "{}: creación, lectura y eliminación del evento de prueba demostradas el {}. Esto no demuestra Google Meet.",
      if connected {
        "Permisos sobre eventos comprobados"
      } else {
        "Comprobación histórica de permisos"
      },
      escape(checked_at)
    ),
  };
  let state = match &view.check {
    CheckView::None => {
      String::from("<p class=\"muted\">Google Meet: sin comprobar para la selección actual.</p>")
    }
    CheckView::Queued => String::from(
      "<p class=\"empty-title\"><strong>Prueba en cola.</strong> Aún no hay un resultado. Actualiza el estado para consultar el avance.</p>",
    ),
    CheckView::Running => String::from(
      "<p class=\"empty-title\"><strong>Prueba en curso.</strong> Se está comprobando el evento y su limpieza; Meet todavía no está confirmado.</p>",
    ),
    CheckView::Passed { checked_at } => format!(
      "<p class=\"empty-title\"><strong>{}</strong></p><p class=\"muted\">Comprobada el: {}. Este resultado corresponde a la prueba actual; no resuelve limpiezas de pruebas anteriores.</p>",
      if connected {
        "Prueba de Google Meet y limpieza completadas."
      } else {
        "Resultado histórico: la prueba de Google Meet y su limpieza se completaron. No confirma disponibilidad actual."
      },
      escape(checked_at)
    ),
    CheckView::Failed { reason } => format!(
      "<p class=\"empty-title\"><strong>La prueba no confirmó Google Meet.</strong></p><p class=\"muted\">{}</p><p class=\"muted\">Si Google rechazó la creación del evento completo, los permisos siguen sin comprobar; no significa que todas las operaciones presenciales sean imposibles.</p>",
      escape(reason)
    ),
    CheckView::CleanupPending { reason } => format!(
      "<p class=\"empty-title\"><strong>Prueba con limpieza pendiente.</strong> No se confirma la capacidad de video mientras falte resolver su limpieza.</p><p class=\"muted\">{}</p>",
      escape(reason)
    ),
  };
  let action = if let Some(reason) = view.probe_block {
    blocked("No puedes iniciar una prueba ahora", reason)
  } else if !connected {
    String::from(
      "<p class=\"muted\">Necesitas una conexión de Google disponible para iniciar una prueba.</p>",
    )
  } else {
    format!(
      r#"<form class="settings-form" method="post" action="/settings/google/probe" aria-describedby="google-probe-consent google-probe-limits">
{}
<input type="hidden" name="expected_probe_id" value="{}">
<div class="form-actions"><button class="primary-button" type="submit">Crear evento de prueba y comprobar Meet</button></div>
</form>"#,
      version_fields(view),
      escape(view.current_probe_id.unwrap_or(""))
    )
  };
  format!(
    r#"<section class="dashboard-section" aria-labelledby="google-check">
<h2 id="google-check">Permisos y Google Meet</h2>
<p class="muted">{permission}</p>
{state}
<p class="muted" id="google-probe-consent">Al iniciar autorizas crear un evento real de 5 minutos, privado y que no bloquea disponibilidad, en el calendario seleccionado. No tendrá invitados, recurrencia ni recordatorios propios. Se intentará eliminar automáticamente, incluso si la prueba falla. No se solicitan avisos a invitados, pero no podemos garantizar que Google no genere notificaciones.</p>
<p class="muted" id="google-probe-limits">Solo puede haber una prueba activa y hasta 10 inicios por hora. No se muestra el enlace de Meet de prueba. Un resultado correcto no habilita citas, video ni pagos para contactos.</p>
{action}
</section>"#
  )
}

fn cleanups(view: &GoogleView<'_>) -> String {
  let mut content = String::new();
  if view.cleanups.is_empty() {
    content.push_str(
      if view.cleanup_previous_page.is_some() || view.cleanup_next_page.is_some() {
        "<p class=\"muted\">No hay limpiezas pendientes en esta página.</p>"
      } else {
        "<p class=\"muted\">No hay eventos de prueba registrados con limpieza pendiente.</p>"
      },
    );
  } else {
    content.push_str("<p class=\"muted\" id=\"google-cleanup-help\">Estas pruebas siguen pendientes aunque cambies de cuenta o una prueba posterior termine bien. Reintentar solo autoriza la limpieza del evento original con la conexión actual: puede que esa cuenta no tenga acceso. No crea otro evento ni habilita Meet. Que un evento no sea accesible no demuestra que se haya eliminado.</p><ul class=\"attention-list\" role=\"list\">");
    for (index, item) in view.cleanups.iter().enumerate() {
      let action = if item.can_retry
        && matches!(view.connection, ConnectionView::Connected)
        && !item.id.is_empty()
        && item.id != "."
        && item.id != ".."
      {
        format!(
          r#"<form class="attention-action" method="post" action="/settings/google/probes/{}/cleanup" aria-labelledby="cleanup-{index}" aria-describedby="google-cleanup-help">
{}
<input type="hidden" name="generation" value="{}">
<button class="primary-button" type="submit">Reintentar solo la limpieza</button>
</form>"#,
          path_segment(&item.id),
          csrf(view),
          view.generation
        )
      } else {
        String::from(
          "<p class=\"muted\">El reintento de limpieza no está disponible ahora. Actualiza el estado para consultar si puedes reintentar.</p>",
        )
      };
      content.push_str(&format!(
        "<li><h3 id=\"cleanup-{index}\">Limpieza pendiente: {}</h3><p class=\"attention-time\">Registrada: {}</p><p class=\"muted\">Referencia de prueba: {}</p><p class=\"muted\">{}</p>{action}</li>",
        escape(&item.calendar_label), escape(&item.occurred_at), escape(&item.id), escape(&item.reason)
      ));
    }
    content.push_str("</ul><p class=\"muted\">Los reintentos aceptados se separan al menos 60 segundos. Eliminar el evento no garantiza la desaparición inmediata del espacio de Meet.</p>");
  }
  if view.cleanup_previous_page.is_some() || view.cleanup_next_page.is_some() {
    content.push_str(
      "<nav class=\"attention-pagination\" aria-label=\"Páginas de limpieza pendiente\">",
    );
    for (page, rel, label) in [
      (view.cleanup_previous_page, "prev", "Página anterior"),
      (view.cleanup_next_page, "next", "Página siguiente"),
    ] {
      if let Some(page) = page {
        content.push_str(&format!("<a class=\"text-link\" href=\"/settings/google?cleanup_page={page}\" rel=\"{rel}\">{label}</a>"));
      }
    }
    content.push_str("</nav>");
  }
  format!(
    "<section class=\"dashboard-section\" aria-labelledby=\"google-cleanup\"><h2 id=\"google-cleanup\">Limpieza pendiente</h2><p class=\"muted\">Hasta 25 registros por página, separados del resultado de la prueba actual.</p>{content}</section>"
  )
}

fn blocked(title: &str, reason: &str) -> String {
  format!(
    "<div class=\"form-error\"><h3>{title}</h3><p>{}</p></div>",
    escape(reason)
  )
}

fn csrf(view: &GoogleView<'_>) -> String {
  format!(
    "<input type=\"hidden\" name=\"csrf_token\" value=\"{}\">",
    escape(view.csrf_token)
  )
}

fn version_fields(view: &GoogleView<'_>) -> String {
  format!(
    "{}<input type=\"hidden\" name=\"generation\" value=\"{}\"><input type=\"hidden\" name=\"selection_version\" value=\"{}\">",
    csrf(view),
    view.generation,
    view.selection_version
  )
}

fn path_segment(value: &str) -> String {
  value
    .bytes()
    .map(|byte| {
      if byte.is_ascii_alphanumeric() || b"-_".contains(&byte) {
        char::from(byte).to_string()
      } else {
        format!("%{byte:02X}")
      }
    })
    .collect()
}
