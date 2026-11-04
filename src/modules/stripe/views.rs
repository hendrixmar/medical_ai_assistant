use crate::modules::doctor::views::{document, escape, navigation};

#[derive(Clone, Copy)]
pub enum Connection {
  NotConfigured,
  Disconnected,
  Incomplete,
  Ready,
}

pub struct SettingsView<'a> {
  pub csrf_token: &'a str,
  pub connection: Connection,
  pub details_submitted: Option<bool>,
  pub notice: Option<&'a str>,
}

pub fn settings(view: SettingsView<'_>) -> String {
  let (title, description) = match view.connection {
    Connection::NotConfigured => (
      "Stripe no está configurado",
      "Pide a quien administra esta instalación que configure Stripe. Las transferencias y la revisión de comprobantes siguen disponibles.",
    ),
    Connection::Disconnected => (
      "Sin conexión de Stripe",
      "Conecta la cuenta de Stripe del doctor para aceptar pagos con tarjeta en citas por video. Las transferencias siguen disponibles.",
    ),
    Connection::Incomplete => (
      "Registro de Stripe incompleto",
      "Continúa el registro en Stripe para habilitar los pagos con tarjeta. La cuenta y los pagos pertenecen al doctor.",
    ),
    Connection::Ready => (
      "Stripe está conectado",
      "Los pagos con tarjeta se depositan directamente en la cuenta de Stripe del doctor. La verificación se realiza al consultar el estado del pago; los comprobantes de transferencia siguen disponibles.",
    ),
  };
  let notice = view
    .notice
    .map(|text| {
      format!(
        "<p class=\"settings-message\" role=\"status\">{}</p>",
        escape(text)
      )
    })
    .unwrap_or_default();
  let details = view
    .details_submitted
    .map(|submitted| {
      format!(
        "<p class=\"muted\">Detalles enviados a Stripe: {}.</p>",
        if submitted { "sí" } else { "no" }
      )
    })
    .unwrap_or_default();
  let connect = match view.connection {
    Connection::NotConfigured | Connection::Ready => String::new(),
    Connection::Disconnected | Connection::Incomplete => {
      let label = if matches!(view.connection, Connection::Disconnected) {
        "Conectar Stripe"
      } else {
        "Continuar registro en Stripe"
      };
      format!(
        r#"<form class="settings-form" method="post" action="/settings/stripe/connect">
<input type="hidden" name="csrf_token" value="{}">
<div class="form-actions"><button class="primary-button" type="submit">{label}</button></div></form>"#,
        escape(view.csrf_token)
      )
    }
  };
  let disconnect = if matches!(view.connection, Connection::Incomplete | Connection::Ready) {
    format!(
      r#"<form class="settings-form" method="post" action="/settings/stripe/disconnect">
<input type="hidden" name="csrf_token" value="{}">
<p class="muted">Desconectar olvida aquí la cuenta vinculada, pero no elimina ni cierra la cuenta de Stripe del doctor. Se intentará desactivar los enlaces de pago abiertos; Stripe no realizará reembolsos automáticamente.</p>
<div class="form-actions"><button type="submit">Desconectar Stripe</button></div></form>"#,
      escape(view.csrf_token)
    )
  } else {
    String::new()
  };
  document(
    "Conexión de Stripe",
    &format!(
      r#"{}
<main id="contenido" class="page-shell" tabindex="-1">
<h1>Conexión de Stripe</h1>
<p class="intro">Conecta Stripe para ofrecer pagos con tarjeta en consultas por video. No se necesita autorización de Stripe para mantener disponibles las transferencias bancarias y sus comprobantes.</p>
{notice}
<section class="dashboard-section" aria-labelledby="stripe-connection">
<h2 id="stripe-connection">{title}</h2>
<p class="muted">{description}</p>
{details}{connect}{disconnect}
</section>
<div class="doctor-number-links"><a class="text-link" href="/settings/stripe">Actualizar estado</a><a class="text-link" href="/settings">Volver a configuración</a></div>
<p class="muted">Al volver de Stripe se consulta de nuevo si la cuenta puede aceptar cargos. Esta página no se actualiza sola.</p>
</main>"#,
      navigation(view.csrf_token, true)
    ),
  )
}
