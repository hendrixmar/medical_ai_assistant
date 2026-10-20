use serde_json::Value;

pub const MENU: &str =
  "1 Agendar\n2 Cambiar cita\n3 Cancelar\n4 Enviar comprobante\n5 Hablar con el doctor";

pub fn classify(envelope: &Value) -> Option<&'static str> {
  let p = &envelope["payload"];
  if p.pointer("/_data/Info/IsGroup").and_then(Value::as_bool) == Some(true) {
    return None;
  }
  let raw = p.pointer("/_data/Message").and_then(Value::as_object);
  if p.pointer("/_data/IsLottieSticker").and_then(Value::as_bool) == Some(true)
    || raw.is_some_and(|r| {
      [
        "stickerMessage",
        "lottieStickerMessage",
        "audioStickerMessage",
        "reactionMessage",
      ]
      .iter()
      .any(|key| r.contains_key(*key))
    })
  {
    return None;
  }
  let Some(raw) = raw else {
    return Some("unsupported");
  };
  let content: Vec<_> = raw
    .keys()
    .filter(|key| key.as_str() != "messageContextInfo")
    .collect();
  if content.len() != 1 {
    return Some("unsupported");
  }
  let key = content[0].as_str();
  if key == "audioMessage" && raw[key].is_object() {
    return Some("audio");
  }
  if key == "documentMessage" && raw[key].is_object() {
    let mime = raw[key]["mimetype"]
      .as_str()
      .or_else(|| p.pointer("/media/mimetype").and_then(Value::as_str));
    return Some(if mime.is_some_and(|m| m.starts_with("audio/")) {
      "audio"
    } else {
      "unsupported"
    });
  }
  let is_text = (key == "conversation" && raw[key].is_string())
    || (key == "extendedTextMessage" && raw[key]["text"].is_string());
  if !is_text || p["hasMedia"].as_bool() == Some(true) || p["media"].is_object() {
    return Some("unsupported");
  }
  Some(match p["body"].as_str().unwrap_or_default().trim() {
    "5" => "followup",
    "1" | "2" | "3" | "4" => "unavailable",
    _ => "menu",
  })
}

/// Only the existing strict raw-text classification may supply challenge text.
pub fn genuine_text(envelope: &Value) -> Option<&str> {
  if !matches!(
    classify(envelope),
    Some("menu" | "followup" | "unavailable")
  ) {
    return None;
  }
  let raw = envelope.pointer("/payload/_data/Message")?;
  let text = raw["conversation"]
    .as_str()
    .or_else(|| raw["extendedTextMessage"]["text"].as_str())?;
  (envelope["payload"]["body"].as_str() == Some(text)).then_some(text.trim())
}

pub(crate) fn privacy_notice(origin: &str) -> String {
  format!("Aviso de privacidad PROVISIONAL: {origin}/aviso-de-privacidad\nEste canal atiende asuntos administrativos. No envíe información clínica.\n\n")
}

pub fn render(template: &str, privacy: bool, origin: &str) -> String {
  let prefix = if privacy {
    privacy_notice(origin)
  } else {
    String::new()
  };
  let doctor_body = match template {
    "doctor_verified" => Some("Tu número de WhatsApp quedó verificado. Esto no confirma la entrega de notificaciones ni habilita envíos automáticos. WhatsApp es de solo lectura; puedes consultar la agenda. Para cambios, usa el panel autenticado."),
    "doctor_read_only" => Some("Tu WhatsApp es de solo lectura. No cambié citas, pagos ni otros registros. Puedes consultar la agenda con «agenda hoy», «agenda mañana» o una fecha AAAA-MM-DD. Para decisiones, usa el panel autenticado."),
    "doctor_audio" => Some("No puedo escuchar audios; por favor escríbame. Tu WhatsApp es de solo lectura. Para consultar citas escribe «agenda hoy»; para decisiones, usa el panel autenticado."),
    _ => None,
  };
  if let Some(body) = doctor_body {
    return format!("{prefix}{body}\n\nPanel: {origin}/dashboard");
  }
  let body = match template {
    "audio" => "No puedo escuchar audios; por favor escríbame",
    "unsupported" => "Para mensajes, por favor escríbame en texto, sin información clínica. Los comprobantes de citas de video deben ser JPEG, PNG, WebP o PDF de hasta 10 MiB.",
    "unavailable" => "El consultorio aún no completó la configuración necesaria para esta opción. Si usted necesita atención personal, escriba 5. No envíe comprobantes sin una cita de video con pago pendiente.",
    "followup" => "Su solicitud de atención personal quedó registrada para el doctor. Esto no garantiza una respuesta inmediata.",
    "resolved" => "El doctor marcó su solicitud de atención personal como atendida. Este registro no confirma que haya conversado con usted.",
    _ => "Por favor, escriba el número de la opción que usted necesita.",
  };
  format!("{prefix}{body}\n\n{MENU}")
}
