use crate::modules::doctor::views::{document, escape, navigation};

pub enum Status {
  NotConfigured,
  Checking,
  Ready,
  Unauthorized,
  Unavailable,
}

pub fn settings(csrf_token: &str, status: Status, checked_at: Option<&str>) -> String {
  let (title, description) = match status {
    Status::NotConfigured => (
      "Mercado Pago no está configurado",
      "Quien administra esta instalación debe colocar el token de acceso de la cuenta de Mercado Pago del doctor. Las transferencias y la revisión de comprobantes siguen disponibles.",
    ),
    Status::Checking => (
      "Verificando Mercado Pago",
      "El servidor todavía no comprueba el token de acceso. Los enlaces de pago se ofrecerán cuando la verificación termine.",
    ),
    Status::Ready => (
      "Mercado Pago está listo",
      "Las citas por video nuevas incluyen un enlace de pago con tarjeta o saldo de Mercado Pago. El dinero llega directamente a la cuenta del doctor y el pago sólo se confirma tras consultarlo en Mercado Pago.",
    ),
    Status::Unauthorized => (
      "Mercado Pago rechazó el token",
      "No se crean enlaces de pago nuevos. Pide a quien administra esta instalación que revise el token de acceso. Las transferencias siguen disponibles.",
    ),
    Status::Unavailable => (
      "No se pudo verificar Mercado Pago",
      "Se reintentará automáticamente. Mientras tanto no se crean enlaces de pago nuevos; las transferencias siguen disponibles.",
    ),
  };
  let checked = checked_at
    .map(|time| {
      format!(
        "<p class=\"muted\">Última verificación: {}.</p>",
        escape(time)
      )
    })
    .unwrap_or_default();
  document(
    "Mercado Pago",
    &format!(
      r#"{}
<main id="contenido" class="page-shell" tabindex="-1">
<h1>Mercado Pago</h1>
<p class="intro">Pago en línea opcional para consultas por video: sólo tarjeta y saldo de Mercado Pago. Un enlace de pago nunca prueba que el dinero llegó.</p>
<section class="dashboard-section" aria-labelledby="mercado-pago-status">
<h2 id="mercado-pago-status">{title}</h2>
<p class="muted">{description}</p>
{checked}
</section>
<div class="doctor-number-links"><a class="text-link" href="/settings/mercado-pago">Actualizar estado</a><a class="text-link" href="/settings">Volver a configuración</a></div>
<p class="muted">Los reembolsos sólo se hacen cuando el doctor pulsa «Reembolsar con Mercado Pago» en la cita. El token de acceso nunca se muestra aquí.</p>
</main>"#,
      navigation(csrf_token, true)
    ),
  )
}
