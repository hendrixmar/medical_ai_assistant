use std::collections::BTreeMap;

use crate::modules::doctor::views::{document, escape, navigation};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Step {
  Details,
  Services,
  Schedule,
  Payments,
}

pub struct ServiceView {
  pub id: String,
  pub name: String,
  pub duration_minutes: String,
  pub price_mxn: String,
}

pub struct SettingsView<'a> {
  pub csrf_token: &'a str,
  pub step: Step,
  pub fields: &'a BTreeMap<String, String>,
  pub services: &'a [ServiceView],
  pub editing_service: bool,
  pub errors: &'a [(String, String)],
  pub message: Option<&'a str>,
  pub complete: [bool; 4],
  pub timezone_options: &'a [(String, String)],
  pub timezone_example: Option<&'a str>,
}

pub fn settings(view: SettingsView<'_>) -> String {
  let steps = [
    (Step::Details, "details", "Datos del consultorio"),
    (Step::Services, "services", "Servicios"),
    (Step::Schedule, "schedule", "Horarios"),
    (Step::Payments, "payments", "Pagos de video"),
  ];
  let mut navigation_steps = String::new();
  for (index, (step, key, label)) in steps.iter().enumerate() {
    let current = if *step == view.step {
      " aria-current=\"step\""
    } else {
      ""
    };
    let state = if view.complete[index] {
      "Listo"
    } else {
      "Por completar"
    };
    navigation_steps.push_str(&format!(
      "<li><a href=\"/settings?step={key}\"{current}><span>{label}</span><span class=\"step-state\">{state}</span></a></li>"
    ));
  }
  let message = view
    .message
    .map(|text| {
      format!(
        "<p class=\"settings-message\" role=\"status\">{}</p>",
        escape(text)
      )
    })
    .unwrap_or_default();
  let content = match view.step {
    Step::Details => details(&view),
    Step::Services => services(&view),
    Step::Schedule => schedule(&view),
    Step::Payments => payments(&view),
  };
  document(
    "Configuración",
    &format!(
      r#"{}
<main id="contenido" class="page-shell" tabindex="-1">
<h1>Configuración</h1>
<p class="intro">Completa los datos de tu consultorio paso a paso. Puedes volver aquí para editarlos. Cada paso guarda sus propios cambios.</p>
<nav class="settings-steps" aria-label="Pasos de configuración"><ol>{navigation_steps}</ol></nav>
{message}{}
{content}
<section class="dashboard-section connections-note" aria-labelledby="connections-title">
<h2 id="connections-title">Conexiones</h2>
<ul><li><a class="text-link" href="/settings/whatsapp">Conexión de WhatsApp del consultorio</a></li><li><a class="text-link" href="/settings/doctor-number">Verificar tu número de WhatsApp</a></li><li><a class="text-link" href="/settings/google">Conexión de Google Calendar</a></li></ul>
<p class="muted">Guardar esta configuración no habilita las consultas por video ni crea citas. Puedes consultar las conexiones de WhatsApp y Google y verificar tu propio número.</p>
</section>
<a class="text-link" href="/dashboard">Volver al panel</a>
</main>"#,
      navigation(view.csrf_token, true),
      error_summary(&view)
    ),
  )
}

fn error_summary(view: &SettingsView<'_>) -> String {
  if view.errors.is_empty() {
    return String::new();
  }
  let mut items = String::new();
  for (key, message) in view.errors {
    let message = escape(message);
    if key.is_empty() || key == "service_id" {
      items.push_str(&format!("<li>{message}</li>"));
    } else {
      items.push_str(&format!(
        "<li><a href=\"#{}\">{message}</a></li>",
        escape(key)
      ));
    }
  }
  format!(
    r#"<div class="form-error" id="settings-errors" role="alert" tabindex="-1" autofocus><h2>Revisa los datos de este paso</h2><p>No se guardaron los cambios. Corrige lo indicado y vuelve a guardar.</p><ul>{items}</ul></div>"#
  )
}

impl SettingsView<'_> {
  fn value(&self, key: &str) -> &str {
    self.fields.get(key).map(String::as_str).unwrap_or("")
  }

  fn attributes(&self, key: &str) -> String {
    let invalid = self.errors.iter().any(|(field, _)| field == key);
    let key = escape(key);
    if invalid {
      format!("aria-invalid=\"true\" aria-describedby=\"{key}-help {key}-error\"")
    } else {
      format!("aria-describedby=\"{key}-help\"")
    }
  }

  // Only authored control markup enters here; all field values are escaped at insertion.
  fn field(&self, key: &str, label: &str, help: &str, control: String) -> String {
    let errors: Vec<_> = self
      .errors
      .iter()
      .filter(|(field, _)| field == key)
      .map(|(_, message)| escape(message))
      .collect();
    let key = escape(key);
    let error = if errors.is_empty() {
      String::new()
    } else {
      format!(
        "<p class=\"field-error\" id=\"{key}-error\">{}</p>",
        errors.join(" ")
      )
    };
    format!(
      "<div class=\"field\"><label for=\"{key}\">{}</label><p class=\"field-hint\" id=\"{key}-help\">{}</p>{control}{error}</div>",
      escape(label),
      escape(help)
    )
  }

  fn input(&self, key: &str, label: &str, help: &str, native: &str) -> String {
    // Number controls hide malformed values; retain the submitted text in visible help on errors.
    let help = if native.contains("type=\"number\"")
      && self.errors.iter().any(|(field, _)| field == key)
      && !self.value(key).is_empty()
    {
      format!("{help} Valor enviado: {}", self.value(key))
    } else {
      help.to_owned()
    };
    self.field(
      key,
      label,
      &help,
      format!(
        "<input id=\"{}\" name=\"{}\" value=\"{}\" {} {native}>",
        escape(key),
        escape(key),
        escape(self.value(key)),
        self.attributes(key)
      ),
    )
  }

  fn textarea(&self, key: &str, label: &str, help: &str) -> String {
    self.field(
      key,
      label,
      help,
      format!(
        "<textarea id=\"{}\" name=\"{}\" rows=\"4\" {} required>{}</textarea>",
        escape(key),
        escape(key),
        self.attributes(key),
        escape(self.value(key))
      ),
    )
  }

  fn form(
    &self,
    action: &str,
    fields: String,
    submit: &str,
    previous: Option<(&str, &str)>,
  ) -> String {
    let described_by = if self.errors.is_empty() {
      ""
    } else {
      " aria-describedby=\"settings-errors\""
    };
    let previous = previous
      .map(|(step, label)| {
        format!(
          "<a class=\"text-link\" href=\"/settings?step={}\">{}</a>",
          escape(step),
          escape(label)
        )
      })
      .unwrap_or_default();
    format!(
      r#"<form class="settings-form" method="post" action="{}" aria-labelledby="step-title"{described_by}>
<input type="hidden" name="csrf_token" value="{}">
{fields}<div class="form-actions"><button class="primary-button" type="submit">{}</button>{previous}</div></form>"#,
      escape(action),
      escape(self.csrf_token),
      escape(submit)
    )
  }
}

fn option(value: &str, label: &str, selected: &str) -> String {
  let selected = if value == selected { " selected" } else { "" };
  format!(
    "<option value=\"{}\"{selected}>{}</option>",
    escape(value),
    escape(label)
  )
}

fn details(view: &SettingsView<'_>) -> String {
  let mut fields = view.input(
    "name",
    "Nombre del consultorio (opcional)",
    "Déjalo en blanco si no deseas indicar un nombre.",
    "type=\"text\"",
  );
  fields.push_str(&view.textarea(
    "location",
    "Ubicación para consultas presenciales",
    "Indica la dirección y las referencias necesarias para llegar.",
  ));
  let selected = view.value("timezone");
  let mut options = option("", "Elige una zona horaria", selected);
  if !selected.is_empty()
    && !view
      .timezone_options
      .iter()
      .any(|(value, _)| value == selected)
  {
    options.push_str(&option(
      selected,
      &format!("Valor enviado: {selected} (elige una zona válida)"),
      selected,
    ));
  }
  for (value, label) in view.timezone_options {
    options.push_str(&option(value, label, selected));
  }
  fields.push_str(&view.field("timezone", "Zona horaria del consultorio", "Elige explícitamente la zona que corresponde a tu ubicación. Se usará para mostrar las fechas y horas.", format!("<select id=\"timezone\" name=\"timezone\" {} required>{options}</select>", view.attributes("timezone"))));
  if let Some(example) = view.timezone_example {
    fields.push_str(&format!("<p class=\"timezone-example\"><strong>Ejemplo de hora local en {}:</strong> {}. Es una referencia de hora, no una cita.</p>", escape(selected), escape(example)));
  }
  format!(
    "<section class=\"settings-content\" aria-labelledby=\"step-title\"><h2 id=\"step-title\">Datos del consultorio</h2><p class=\"muted\">Todos los campos son obligatorios salvo el nombre.</p>{}</section>",
    view.form(
      "/settings/details",
      fields,
      "Guardar y continuar a servicios",
      None
    )
  )
}

fn services(view: &SettingsView<'_>) -> String {
  let mut list = String::new();
  if view.services.is_empty() {
    list.push_str("<p class=\"muted\">Aún no hay servicios guardados. Añade el nombre, la duración y el precio de una consulta.</p>");
  } else {
    list.push_str("<ul class=\"service-list\">");
    for service in view.services {
      list.push_str(&format!("<li><div><strong>{}</strong><p class=\"muted\">{} minutos · {} MXN</p></div><a class=\"text-link\" href=\"/settings?step=services&amp;service={}\" aria-label=\"Editar {}\">Editar</a></li>", escape(&service.name), escape(&service.duration_minutes), escape(&service.price_mxn), escape(&service.id), escape(&service.name)));
    }
    list.push_str("</ul>");
  }
  let mut fields = format!(
    "<input type=\"hidden\" name=\"service_id\" value=\"{}\">",
    escape(view.value("service_id"))
  );
  fields.push_str(&view.input(
    "service_name",
    "Nombre del servicio",
    "Usa el nombre de la consulta que ofreces.",
    "type=\"text\" required",
  ));
  fields.push_str("<div class=\"field-pair\">");
  fields.push_str(&view.input(
    "duration_minutes",
    "Duración (minutos)",
    "Un número entero mayor que cero.",
    "type=\"number\" min=\"1\" step=\"1\" required",
  ));
  fields.push_str(&view.input(
    "price_mxn",
    "Precio (MXN)",
    "Pesos mexicanos, con hasta 2 decimales. Puede ser 0.",
    "type=\"number\" inputmode=\"decimal\" min=\"0\" step=\"0.01\" required",
  ));
  fields.push_str("</div>");
  let (title, submit) = if view.editing_service {
    ("Editar servicio", "Guardar cambios del servicio")
  } else {
    ("Añadir servicio", "Añadir servicio")
  };
  let add = if view.editing_service {
    "<a class=\"text-link\" href=\"/settings?step=services\">Añadir otro servicio</a>"
  } else {
    ""
  };
  format!(
    "<section class=\"settings-content\" aria-labelledby=\"services-title\"><h2 id=\"services-title\">Servicios</h2>{list}<h2 class=\"form-heading\" id=\"step-title\">{title}</h2><p class=\"muted\">Todos los campos son obligatorios. Guarda el servicio antes de continuar.</p>{}{add}<a class=\"text-link continue-link\" href=\"/settings?step=schedule\">Continuar a horarios</a></section>",
    view.form(
      "/settings/services",
      fields,
      submit,
      Some(("details", "Volver a datos del consultorio"))
    )
  )
}

fn schedule(view: &SettingsView<'_>) -> String {
  let mut fields = String::from(
    "<p class=\"field-hint\">Indica si atiendes cada día. Si eliges Cerrado, deja sus horarios en blanco. Los días sin configurar mantienen este paso pendiente.</p><p class=\"field-hint\">Para varios intervalos, sepáralos con comas, sin superponerlos. Usa HH:MM-HH:MM en formato de 24 horas. Un intervalo no cruza medianoche: divídelo entre los dos días; 24:00 solo se admite como hora de fin.</p>",
  );
  for (day, label) in [
    ("mon", "Lunes"),
    ("tue", "Martes"),
    ("wed", "Miércoles"),
    ("thu", "Jueves"),
    ("fri", "Viernes"),
    ("sat", "Sábado"),
    ("sun", "Domingo"),
  ] {
    let status = format!("{day}_status");
    let hours = format!("{day}_hours");
    let selected = view.value(&status);
    let mut options = option("", "Sin configurar", selected);
    if !["", "open", "closed"].contains(&selected) {
      options.push_str(&option(
        selected,
        &format!("Valor no válido: {selected}"),
        selected,
      ));
    }
    options.push_str(&option("open", "Abierto", selected));
    options.push_str(&option("closed", "Cerrado", selected));
    fields.push_str(&format!(
      "<fieldset class=\"weekday\"><legend>{label}</legend><div class=\"day-fields\">"
    ));
    fields.push_str(&view.field(
      &status,
      "Atención",
      "Elige una opción para este día.",
      format!(
        "<select id=\"{status}\" name=\"{status}\" {}>{options}</select>",
        view.attributes(&status)
      ),
    ));
    fields.push_str(&view.input(
      &hours,
      "Horarios",
      "HH:MM-HH:MM, separados por comas. Cerrado: deja en blanco.",
      "type=\"text\"",
    ));
    fields.push_str("</div></fieldset>");
  }
  fields.push_str("<h3 class=\"form-heading\">Reglas de agenda</h3><div class=\"field-pair\">");
  for (key, label, help, minimum) in [
    (
      "buffer_before_minutes",
      "Tiempo antes de cada cita (minutos)",
      "Tiempo reservado antes de la consulta. Puede ser 0.",
      "0",
    ),
    (
      "buffer_after_minutes",
      "Tiempo después de cada cita (minutos)",
      "Tiempo reservado después de la consulta. Puede ser 0.",
      "0",
    ),
    (
      "minimum_notice_minutes",
      "Anticipación mínima (minutos)",
      "Cuánto tiempo antes se puede solicitar una cita. Puede ser 0.",
      "0",
    ),
    (
      "grid_minutes",
      "Intervalo entre horas de inicio (minutos)",
      "Separación entre las horas de inicio que se ofrecerán. Mayor que 0.",
      "1",
    ),
    (
      "horizon_days",
      "Horizonte de agenda (días)",
      "Hasta cuántos días hacia adelante se ofrecerán citas. Mayor que 0.",
      "1",
    ),
  ] {
    fields.push_str(&view.input(
      key,
      label,
      help,
      &format!("type=\"number\" min=\"{minimum}\" step=\"1\" required"),
    ));
  }
  fields.push_str("</div>");
  fields.push_str(&view.field(
    "reminder_2h_enabled",
    "Recordatorio adicional dos horas antes",
    "Es opcional; el recordatorio predeterminado se envía 24 horas antes de una cita confirmada.",
    format!(
      "<input id=\"reminder_2h_enabled\" name=\"reminder_2h_enabled\" type=\"checkbox\" value=\"true\" {} {}>",
      if view.value("reminder_2h_enabled") == "true" {
        "checked"
      } else {
        ""
      },
      view.attributes("reminder_2h_enabled")
    ),
  ));
  fields.push_str(&view.textarea(
    "cancellation_policy",
    "Política de cancelación",
    "Escribe la política de tu consultorio que se mostrará antes de cancelar una cita.",
  ));
  format!(
    "<section class=\"settings-content\" aria-labelledby=\"step-title\"><h2 id=\"step-title\">Horarios</h2><p class=\"muted\">Configura la semana y tus reglas de agenda. Las reglas y la política son obligatorias.</p>{}</section>",
    view.form(
      "/settings/schedule",
      fields,
      "Guardar y continuar a pagos",
      Some(("services", "Volver a servicios"))
    )
  )
}

fn payments(view: &SettingsView<'_>) -> String {
  let mut fields = view.input(
    "bank",
    "Banco",
    "Nombre del banco que recibirá las transferencias.",
    "type=\"text\" required",
  );
  fields.push_str(&view.input(
    "beneficiary",
    "Beneficiario",
    "Nombre del titular de la cuenta.",
    "type=\"text\" required",
  ));
  fields.push_str(&view.input("clabe", "CLABE", "18 dígitos, sin espacios. Se conservan los ceros iniciales; el formato no verifica la cuenta bancaria.", "type=\"text\" inputmode=\"numeric\" pattern=\"[0-9]{18}\" minlength=\"18\" maxlength=\"18\" required"));
  fields.push_str(&view.input("receipt_window_minutes", "Plazo para enviar el comprobante (minutos)", "Elige cuánto tiempo habrá desde la reserva para enviarlo. La hora límite también estará acotada por el margen de revisión antes de la cita. Mayor que 0.", "type=\"number\" min=\"1\" step=\"1\" required"));
  fields.push_str(&view.input("review_margin_minutes", "Margen de revisión antes de la cita (minutos)", "Tiempo que se reservará antes de la cita para revisar el comprobante. 180 minutos equivalen a 3 horas. Puede ser 0.", "type=\"number\" min=\"0\" step=\"1\" required"));
  fields.push_str(&view.input("grace_minutes", "Tolerancia de recepción (minutos)", "Cubre demoras en la recepción de un comprobante enviado a tiempo. Nunca amplía la hora límite para enviarlo. Puede ser 0.", "type=\"number\" min=\"0\" step=\"1\" required"));
  fields.push_str("<h3 class=\"form-heading\">Avisos de comprobantes</h3>");
  fields.push_str(&view.input(
    "review_interval_minutes",
    "Intervalo entre avisos de revisión (minutos)",
    "Se repite mientras haya un comprobante sin revisar y solo dentro del horario del consultorio. Mayor que 0.",
    "type=\"number\" min=\"1\" step=\"1\" required",
  ));
  fields.push_str(&view.input(
    "urgent_notice_minutes",
    "Aviso urgente antes de la cita (minutos)",
    "Cuánto tiempo antes de la cita se envía el aviso urgente mientras el comprobante siga sin revisar. Mayor que 0.",
    "type=\"number\" min=\"1\" step=\"1\" required",
  ));
  format!(
    "<section class=\"settings-content\" aria-labelledby=\"step-title\"><h2 id=\"step-title\">Pagos de video</h2><p class=\"muted\">Todos los campos son obligatorios. Estos datos se guardan para las futuras instrucciones de transferencia; no habilitan video ni confirman pagos.</p>{}</section>",
    view.form(
      "/settings/payments",
      fields,
      "Guardar datos de pago",
      Some(("schedule", "Volver a horarios"))
    )
  )
}
