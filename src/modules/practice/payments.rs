use super::model::{number, text, value, Errors, Fields};

pub struct Payments {
  pub bank: String,
  pub beneficiary: String,
  pub clabe: String,
  pub receipt_window_minutes: i32,
  pub review_margin_minutes: i32,
  pub grace_minutes: i32,
  pub review_interval_minutes: i32,
  pub urgent_notice_minutes: i32,
}

impl Payments {
  pub fn parse(fields: &Fields) -> Result<Self, Errors> {
    let mut errors = vec![];
    let bank = text(fields, "bank", 120, true, &mut errors);
    let beneficiary = text(fields, "beneficiary", 200, true, &mut errors);
    let clabe = value(fields, "clabe");
    if clabe.len() != 18 || !clabe.bytes().all(|b| b.is_ascii_digit()) {
      errors.push(("clabe".into(), "Escribe exactamente 18 dígitos del 0 al 9, sin espacios. Solo se comprueba el formato, no la cuenta ni la titularidad.".into()));
    }
    let result = Self {
      bank,
      beneficiary,
      clabe: clabe.into(),
      receipt_window_minutes: number(fields, "receipt_window_minutes", true, &mut errors),
      review_margin_minutes: number(fields, "review_margin_minutes", false, &mut errors),
      grace_minutes: number(fields, "grace_minutes", false, &mut errors),
      review_interval_minutes: number(fields, "review_interval_minutes", true, &mut errors),
      urgent_notice_minutes: number(fields, "urgent_notice_minutes", true, &mut errors),
    };
    if errors.is_empty() {
      Ok(result)
    } else {
      Err(errors)
    }
  }
}
