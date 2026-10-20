use std::collections::BTreeMap;

pub type Fields = BTreeMap<String, String>;
pub type Errors = Vec<(String, String)>;

#[derive(serde::Deserialize)]
pub struct Practice {
  pub name: Option<String>,
  pub location: Option<String>,
  pub timezone: Option<String>,
  pub weekly_hours: Option<[super::schedule::Day; 7]>,
  pub buffer_before_minutes: Option<i32>,
  pub buffer_after_minutes: Option<i32>,
  pub minimum_notice_minutes: Option<i32>,
  pub grid_minutes: Option<i32>,
  pub horizon_days: Option<i32>,
  pub cancellation_policy: Option<String>,
  pub bank: Option<String>,
  pub beneficiary: Option<String>,
  pub clabe: Option<String>,
  pub receipt_window_minutes: Option<i32>,
  pub review_margin_minutes: Option<i32>,
  pub grace_minutes: Option<i32>,
  pub reminder_2h_enabled: bool,
  pub review_interval_minutes: i32,
  pub urgent_notice_minutes: i32,
}

pub struct Details {
  pub name: Option<String>,
  pub location: String,
  pub timezone: chrono_tz::Tz,
}

pub fn value<'a>(fields: &'a Fields, key: &str) -> &'a str {
  fields
    .get(key)
    .map(String::as_str)
    .unwrap_or_default()
    .trim()
}

pub(super) fn text(
  fields: &Fields,
  key: &str,
  max: usize,
  required: bool,
  errors: &mut Errors,
) -> String {
  let text = value(fields, key);
  if required && text.is_empty() {
    errors.push((key.into(), "Completa este campo.".into()));
  } else if text.chars().count() > max || text.contains('\0') {
    errors.push((
      key.into(),
      format!("Usa hasta {max} caracteres, sin caracteres nulos."),
    ));
  }
  text.into()
}

impl Details {
  pub fn parse(fields: &Fields) -> Result<Self, Errors> {
    let mut errors = vec![];
    let name = text(fields, "name", 120, false, &mut errors);
    let location = text(fields, "location", 500, true, &mut errors);
    let timezone = value(fields, "timezone").parse::<chrono_tz::Tz>();
    if timezone.is_err() {
      errors.push((
        "timezone".into(),
        "Elige una zona horaria IANA válida explícitamente.".into(),
      ));
    }
    if !errors.is_empty() {
      return Err(errors);
    }
    Ok(Self {
      name: (!name.is_empty()).then_some(name),
      location,
      timezone: timezone.unwrap(),
    })
  }
}

#[derive(sea_orm::FromQueryResult)]
pub struct Service {
  pub id: uuid::Uuid,
  pub name: String,
  pub duration_minutes: i32,
  pub price_centavos: i64,
}

pub(super) fn number(fields: &Fields, key: &str, positive: bool, errors: &mut Errors) -> i32 {
  let raw = value(fields, key);
  match raw.parse::<i32>() {
    Ok(n) if raw.bytes().all(|b| b.is_ascii_digit()) && n >= i32::from(positive) => n,
    _ => {
      errors.push((
        key.into(),
        format!(
          "Escribe un entero entre {} y 2147483647.",
          i32::from(positive)
        ),
      ));
      0
    }
  }
}

fn centavos(raw: &str) -> Option<i64> {
  let (whole, fraction) = raw.split_once('.').map_or((raw, ""), |parts| parts);
  if whole.is_empty()
    || !whole.bytes().all(|b| b.is_ascii_digit())
    || fraction.len() > 2
    || !fraction.bytes().all(|b| b.is_ascii_digit())
    || (raw.contains('.') && fraction.is_empty())
  {
    return None;
  }
  let pesos = whole.parse::<i64>().ok()?;
  let cents = match fraction.len() {
    0 => 0,
    1 => fraction.parse::<i64>().ok()? * 10,
    _ => fraction.parse::<i64>().ok()?,
  };
  pesos.checked_mul(100)?.checked_add(cents)
}

impl Service {
  pub fn parse(fields: &Fields) -> Result<Self, Errors> {
    let mut errors = vec![];
    let id = value(fields, "service_id").parse::<uuid::Uuid>();
    if id.is_err() {
      errors.push((
        "service_id".into(),
        "Identificador inválido. Abre un formulario de servicio nuevo.".into(),
      ));
    }
    let name = text(fields, "service_name", 120, true, &mut errors);
    let duration_minutes = number(fields, "duration_minutes", true, &mut errors);
    let price_centavos = centavos(value(fields, "price_mxn"));
    if price_centavos.is_none() {
      errors.push(("price_mxn".into(), "Escribe un precio MXN no negativo, con punto y hasta 2 decimales; máximo 92233720368547758.07.".into()));
    }
    if !errors.is_empty() {
      return Err(errors);
    }
    Ok(Self {
      id: id.unwrap(),
      name,
      duration_minutes,
      price_centavos: price_centavos.unwrap(),
    })
  }

  pub fn price_mxn(&self) -> String {
    format!(
      "{}.{:02}",
      self.price_centavos / 100,
      self.price_centavos % 100
    )
  }

  pub fn fields(&self) -> Fields {
    [
      ("service_id", self.id.to_string()),
      ("service_name", self.name.clone()),
      ("duration_minutes", self.duration_minutes.to_string()),
      ("price_mxn", self.price_mxn()),
    ]
    .into_iter()
    .map(|(k, v)| (k.into(), v))
    .collect()
  }
}

impl Practice {
  pub fn fields(&self) -> Fields {
    let mut fields: Fields = [
      ("name", self.name.clone().unwrap_or_default()),
      ("location", self.location.clone().unwrap_or_default()),
      ("timezone", self.timezone.clone().unwrap_or_default()),
      ("bank", self.bank.clone().unwrap_or_default()),
      ("beneficiary", self.beneficiary.clone().unwrap_or_default()),
      ("clabe", self.clabe.clone().unwrap_or_default()),
      (
        "cancellation_policy",
        self.cancellation_policy.clone().unwrap_or_default(),
      ),
      ("reminder_2h_enabled", self.reminder_2h_enabled.to_string()),
      (
        "review_interval_minutes",
        self.review_interval_minutes.to_string(),
      ),
      (
        "urgent_notice_minutes",
        self.urgent_notice_minutes.to_string(),
      ),
    ]
    .into_iter()
    .map(|(k, v)| (k.into(), v))
    .collect();
    for (key, value) in [
      ("buffer_before_minutes", self.buffer_before_minutes),
      ("buffer_after_minutes", self.buffer_after_minutes),
      ("minimum_notice_minutes", self.minimum_notice_minutes),
      ("grid_minutes", self.grid_minutes),
      ("horizon_days", self.horizon_days),
      ("receipt_window_minutes", self.receipt_window_minutes),
      ("review_margin_minutes", self.review_margin_minutes),
      ("grace_minutes", self.grace_minutes),
    ] {
      fields.insert(key.into(), value.map(|n| n.to_string()).unwrap_or_default());
    }
    super::schedule::fields(&self.weekly_hours.clone().unwrap_or_default(), &mut fields);
    fields
  }
  pub fn complete(&self) -> [bool; 4] {
    let fields = self.fields();
    [
      Details::parse(&fields).is_ok(),
      false,
      super::schedule::Schedule::parse(&fields).is_ok(),
      super::payments::Payments::parse(&fields).is_ok(),
    ]
  }
}
