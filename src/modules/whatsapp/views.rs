use crate::modules::doctor::views::{document, escape, navigation};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ConnectionState {
  Unconfigured,
  Unavailable,
  Stopped,
  Starting,
  Qr,
  PasskeyRequired,
  PasskeyConfirmationRequired,
  Working,
  Failed,
  Unknown,
}

pub struct ConnectionView<'a> {
  pub csrf_token: &'a str,
  pub state: ConnectionState,
  pub phone: Option<&'a str>,
  pub notice: Option<&'a str>,
  pub pairing_code: Option<&'a str>,
  pub pairing_phone: Option<&'a str>,
}

pub fn connection(view: ConnectionView<'_>) -> String {
  let (title, description) = match view.state {
    ConnectionState::Unconfigured => (
      "Conexión sin configurar",
      "La conexión de WhatsApp del consultorio aún no está preparada. Pide a quien administra esta instalación que complete la configuración.",
    ),
    ConnectionState::Unavailable => (
      "No se pudo consultar la conexión",
      "No podemos confirmar si WhatsApp está conectado. Actualiza el estado para volver a consultar. Si el problema continúa, pide ayuda a quien administra esta instalación.",
    ),
    ConnectionState::Stopped => (
      "Conexión detenida",
      "Inicia la conexión de WhatsApp del consultorio. Si se solicita vincular el teléfono, aquí aparecerá un código QR.",
    ),
    ConnectionState::Starting => (
      "Conexión iniciándose",
      "WhatsApp está iniciando la conexión; todavía no ha confirmado que esté conectado. Espera un momento y actualiza el estado.",
    ),
    ConnectionState::Qr => (
      "Vincula el teléfono del consultorio",
      "WhatsApp solicita escanear un código QR. La conexión todavía no está confirmada.",
    ),
    ConnectionState::PasskeyRequired => (
      "WhatsApp solicita una clave de acceso",
      "Este paso no se puede completar desde esta página. Sigue las indicaciones de WhatsApp en el teléfono y pide ayuda a quien administra esta instalación para continuar. Después, actualiza el estado.",
    ),
    ConnectionState::PasskeyConfirmationRequired => (
      "WhatsApp solicita confirmar una clave de acceso",
      "La confirmación debe completarse fuera de esta página. Sigue las indicaciones de WhatsApp en el teléfono y pide ayuda a quien administra esta instalación para continuar. Después, actualiza el estado.",
    ),
    ConnectionState::Working => (
      "WhatsApp conectado",
      "WhatsApp reporta que la conexión está activa.",
    ),
    ConnectionState::Failed => (
      "La conexión falló",
      "WhatsApp reporta un fallo. Pide ayuda a quien administra esta instalación para revisar la conexión. Esta página no la reinicia automáticamente.",
    ),
    ConnectionState::Unknown => (
      "Estado de conexión no reconocido",
      "No podemos confirmar si WhatsApp está conectado con el estado recibido. Actualiza el estado; si continúa, pide ayuda a quien administra esta instalación.",
    ),
  };
  let notice = view
    .notice
    .map(|text| {
      format!(
        "<div class=\"form-error\" id=\"connection-notice\" role=\"alert\" tabindex=\"-1\" autofocus>{}</div>",
        escape(text)
      )
    })
    .unwrap_or_default();
  let phone = if view.state == ConnectionState::Working {
    view
      .phone
      .map(|phone| {
        format!(
          "<p class=\"connection-phone\"><strong>Número conectado:</strong> {}</p>",
          escape(phone)
        )
      })
      .unwrap_or_default()
  } else {
    String::new()
  };
  let pairing = if view.state == ConnectionState::Qr {
    let code_block = match view.pairing_code {
      Some(code) => format!(
        r#"<p>Tu código de vinculación:</p>
<p class="doctor-number-code">{}</p>
<ol class="connection-instructions">
<li>Abre WhatsApp en el teléfono del consultorio.</li>
<li>Ve a Dispositivos vinculados y elige vincular con número de teléfono.</li>
<li>Escribe este código.</li>
</ol>
<p class="muted">El código vence pronto y no se guarda en esta página: si actualizas el estado, pide uno nuevo. Pedir un código no confirma la vinculación; actualiza el estado para revisarla.</p>"#,
        escape(code)
      ),
      None => String::new(),
    };
    format!(
      r#"<section class="dashboard-section" aria-labelledby="pairing-code-title">
<h2 id="pairing-code-title">Vincular con un código</h2>
<p class="muted">Si no puedes escanear el código QR, pide un código de vinculación para el número del teléfono del consultorio.</p>
{code_block}
<form class="settings-form" method="post" action="/settings/whatsapp/request-code">
<input type="hidden" name="csrf_token" value="{}">
<div class="field"><label for="phone">Número de teléfono</label><p class="field-hint" id="phone-help">Número con clave de país, solo dígitos.</p><input type="tel" id="phone" name="phone" autocomplete="tel" value="{}" aria-describedby="phone-help" required></div>
<div class="form-actions"><button class="primary-button" type="submit">Pedir código de vinculación</button></div>
</form>
</section>"#,
      escape(view.csrf_token),
      escape(view.pairing_phone.unwrap_or_default())
    )
  } else {
    String::new()
  };
  let action = match view.state {
    ConnectionState::Stopped => {
      let described_by = if view.notice.is_some() {
        " aria-describedby=\"connection-notice\""
      } else {
        ""
      };
      format!(
        r#"<form class="settings-form" method="post" action="/settings/whatsapp/start" aria-label="Conectar WhatsApp"{described_by}>
<input type="hidden" name="csrf_token" value="{}">
<div class="form-actions"><button class="primary-button" type="submit">Conectar WhatsApp</button></div>
</form>"#,
        escape(view.csrf_token)
      )
    }
    ConnectionState::Qr => String::from(
      r#"<ol class="connection-instructions">
<li>Abre WhatsApp en el teléfono del consultorio.</li>
<li>Entra en <strong>Dispositivos vinculados</strong> y elige <strong>Vincular un dispositivo</strong>.</li>
<li>Escanea este código con el teléfono.</li>
</ol>
<img class="connection-qr" src="/settings/whatsapp/qr" width="280" height="280" alt="Código QR para vincular el WhatsApp del consultorio" aria-describedby="qr-help">
<p class="muted" id="qr-help">Si el código no aparece o ya venció, actualiza el estado para volver a consultarlo. Después de escanearlo, actualiza para comprobar la conexión.</p>"#,
    ),
    _ => String::new(),
  };
  document(
    "WhatsApp del consultorio",
    &format!(
      r#"{}
<main id="contenido" class="page-shell" tabindex="-1">
<h1>WhatsApp del consultorio</h1>
<p class="intro">Consulta el estado de la conexión y vincula el teléfono cuando WhatsApp lo solicite.</p>
{notice}
<section class="dashboard-section connection-status" aria-labelledby="connection-title">
<h2 id="connection-title">{title}</h2>
<p class="muted">{description}</p>
{phone}{action}
<a class="text-link" href="/settings/whatsapp">Actualizar estado</a>
<p class="muted">Una conexión activa no habilita por sí sola las respuestas automáticas ni confirma la entrega de mensajes. Consulta los pendientes y el estado del envío automático en el panel.</p>
</section>
{pairing}
<a class="text-link" href="/settings">Volver a configuración</a>
</main>"#,
      navigation(view.csrf_token, true)
    ),
  )
}
