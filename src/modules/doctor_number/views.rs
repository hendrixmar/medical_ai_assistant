use crate::modules::doctor::views::{document, escape, navigation};

pub enum ChallengeView<'a> {
  None,
  Active {
    code: &'a str,
    practice_phone: &'a str,
    expires_at: &'a str,
  },
  Expired,
  Exhausted,
  Used,
}

pub struct DoctorNumberView<'a> {
  pub csrf_token: &'a str,
  pub verified_phone: Option<&'a str>,
  pub verified_at: Option<&'a str>,
  pub practice_phone: Option<&'a str>,
  pub challenge: ChallengeView<'a>,
  pub issue_block: Option<&'a str>,
  pub notice: Option<&'a str>,
  pub load_error: bool,
}

pub fn doctor_number(view: DoctorNumberView<'_>) -> String {
  let content = if view.load_error {
    String::from(
      r#"<div class="form-error" role="alert" tabindex="-1" autofocus><h2>No se pudo cargar la verificación</h2><p>No podemos confirmar tu número ni el estado del código. Actualiza el estado para volver a consultar; no se muestran datos anteriores.</p></div><a class="text-link" href="/settings/doctor-number">Actualizar estado</a>"#,
    )
  } else {
    let notice = view.notice.map(|text| format!(
      "<div class=\"form-error\" id=\"doctor-number-notice\" role=\"alert\" tabindex=\"-1\" autofocus>{}</div>",
      escape(text)
    )).unwrap_or_default();
    let binding = if let Some(phone) = view.verified_phone {
      let verified_at = view
        .verified_at
        .map(|time| format!("<p class=\"muted\">Verificado el: {}</p>", escape(time)))
        .unwrap_or_default();
      format!(
        "<p class=\"connection-phone\"><strong>Tu número verificado:</strong> {}</p>{verified_at}<p class=\"muted\">Si verificas otro número, este se conserva hasta que la nueva verificación se complete correctamente.</p>",
        escape(phone)
      )
    } else {
      String::from("<p class=\"muted\">Todavía no has verificado tu número de WhatsApp.</p>")
    };
    let current_phone = if let Some(phone) = view.practice_phone {
      format!(
        "<p class=\"connection-phone\"><strong>WhatsApp del consultorio consultado ahora:</strong> {}</p><p class=\"muted\">Este es el número que reporta la conexión actual. Un código ya generado conserva el destino indicado junto a él; si cambiaste la cuenta del consultorio, necesitas un nuevo código.</p>",
        escape(phone)
      )
    } else {
      String::from("<p class=\"muted\">No hay un número actual del consultorio confirmado por la conexión de WhatsApp.</p>")
    };
    let action = if let Some(reason) = view.issue_block {
      format!(
        "<div class=\"form-error\"><h3>No puedes generar un código ahora</h3><p>{}</p></div>",
        escape(reason)
      )
    } else {
      let label = if matches!(view.challenge, ChallengeView::None) {
        "Generar código"
      } else {
        "Generar un nuevo código"
      };
      format!(
        r#"<form class="settings-form" method="post" action="/settings/doctor-number/challenge" aria-label="Generar código de verificación" aria-describedby="doctor-number-limits doctor-number-replacement">
<input type="hidden" name="csrf_token" value="{}">
<div class="form-actions"><button class="primary-button" type="submit">{label}</button></div>
</form>"#,
        escape(view.csrf_token)
      )
    };
    format!(
      r#"{notice}
<section class="dashboard-section" aria-labelledby="doctor-number-binding">
<h2 id="doctor-number-binding">Tu número de WhatsApp</h2>
{binding}
</section>
<section class="dashboard-section" aria-labelledby="doctor-number-challenge">
<h2 id="doctor-number-challenge">Verifica desde tu teléfono</h2>
{}
<a class="text-link" href="/settings/doctor-number">Actualizar estado</a>
{current_phone}
<p class="muted" id="doctor-number-replacement">Generar un nuevo código invalida el anterior, pero no cambia tu número verificado.</p>
{action}
<p class="muted" id="doctor-number-limits">Cada código dura 10 minutos. Se invalida tras 5 intentos incorrectos que cumplan los requisitos de verificación, incluso si proceden de otro contacto. Los mensajes sin evidencia válida no consumen intentos. Puedes generar como máximo un código cada 60 segundos y 5 en una hora.</p>
</section>
<section class="dashboard-section" aria-labelledby="doctor-number-scope">
<h2 id="doctor-number-scope">Qué permite esta verificación</h2>
<p class="muted">La verificación usa el número que WhatsApp identifica en el mensaje recibido y la evidencia de que llegó a la cuenta del consultorio guardada con el código. No basta con escribir un número: ambos deben poder identificarse y ser distintos.</p>
<p class="muted">Tu WhatsApp es de solo lectura: puedes consultar tu agenda, pero no cambiar citas, pagos ni otros registros. Haz esos cambios desde el panel autenticado. Las notificaciones dependen de que el envío automático esté habilitado y de la conexión de WhatsApp; verificar el número no confirma su entrega.</p>
</section>"#,
      challenge(&view.challenge)
    )
  };
  document(
    "Verificar tu número de WhatsApp",
    &format!(
      r#"{}
<main id="contenido" class="page-shell doctor-number" tabindex="-1">
<h1>Verificar tu número de WhatsApp</h1>
<p class="intro">Vincula tu propio número, distinto del WhatsApp del consultorio, enviando un código desde tu teléfono.</p>
{content}
<div class="doctor-number-links">
<a class="text-link" href="/settings/whatsapp">Ver conexión de WhatsApp</a>
<a class="text-link" href="/settings">Volver a configuración</a>
</div>
</main>"#,
      navigation(view.csrf_token, true)
    ),
  )
}

fn challenge(view: &ChallengeView<'_>) -> String {
  match view {
    ChallengeView::None => String::from(
      "<p class=\"muted\">Genera un código y envíalo como mensaje de texto desde tu WhatsApp personal al número del consultorio que se indique aquí.</p>",
    ),
    ChallengeView::Active { code, practice_phone, expires_at } => format!(
      r#"<p class="muted">Código de verificación</p>
<p class="doctor-number-code">{}</p>
<p><strong>Envíalo a este número del consultorio:</strong> {}</p>
<p class="muted"><strong>Vence (fecha, hora y zona horaria):</strong> {}</p>
<ol class="connection-instructions">
<li>Abre WhatsApp en tu teléfono personal y escribe al número indicado junto al código.</li>
<li>Envía solo los 6 dígitos como texto, incluidos los ceros iniciales; no como audio ni pie de foto.</li>
<li>Vuelve aquí y pulsa «Actualizar estado» para comprobar la verificación. Esta página no se actualiza sola.</li>
</ol>"#,
      escape(code), escape(practice_phone), escape(expires_at)
    ),
    ChallengeView::Expired => String::from(
      "<p class=\"muted\">El código venció y ya no puede verificar un número. Genera uno nuevo cuando esté disponible.</p>",
    ),
    ChallengeView::Exhausted => String::from(
      "<p class=\"muted\">El código se invalidó tras 5 intentos incorrectos con evidencia válida. Genera uno nuevo cuando esté disponible.</p>",
    ),
    ChallengeView::Used => String::from(
      "<p class=\"muted\">Este código ya se utilizó y no puede volver a usarse. El número verificado guardado se muestra arriba.</p>",
    ),
  }
}
