mod details;
mod payments;
mod persistence;
mod schedule;
mod security;
mod services;

fn changed<'a>(
  mut fields: Vec<(&'a str, &'a str)>,
  key: &'a str,
  value: &'a str,
) -> Vec<(&'a str, &'a str)> {
  fields.retain(|(name, _)| *name != key);
  fields.push((key, value));
  fields
}

fn selected(html: &str, name: &str, value: &str) -> bool {
  html
    .split(&format!("name=\"{name}\""))
    .nth(1)
    .unwrap()
    .split("</select>")
    .next()
    .unwrap()
    .contains(&format!("value=\"{value}\" selected"))
}

use crate::{app::tests::with_database, modules::doctor::tests::*};

async fn page(app: &axum::Router, session: &str, step: &str) -> String {
  let response = authenticated_get(app, &format!("/settings?step={step}"), session).await;
  assert_eq!(response.status(), 200);
  text(response).await
}

async fn save(
  app: &axum::Router,
  session: &str,
  step: &str,
  csrf: &str,
  fields: &[(&str, &str)],
) -> axum::response::Response {
  let mut fields = fields.to_vec();
  fields.push(("csrf_token", csrf));
  post(
    app,
    &format!("/settings/{step}"),
    Some(session),
    Some("https://practice.test"),
    &fields,
  )
  .await
}

#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
async fn details_save_and_reload_through_the_authenticated_router() {
  with_database(|db| async move {
    let (_, _, app, totp) = fixture(&db).await;
    let session = signed_in(&app, &totp, NOW + 30).await;
    let initial = text(authenticated_get(&app, "/settings", &session).await).await;
    let csrf = field(&initial, "csrf_token");
    let response = post(
      &app,
      "/settings/details",
      Some(&session),
      Some("https://practice.test"),
      &[
        ("csrf_token", &csrf),
        ("name", "Consultorio de prueba"),
        ("location", "Ubicación de prueba"),
        ("timezone", "America/Tijuana"),
      ],
    )
    .await;
    assert_eq!(response.status(), 303);
    assert_eq!(
      response.headers()["location"],
      "/settings?step=services&saved=1"
    );
    let saved = text(authenticated_get(&app, "/settings?step=details", &session).await).await;
    assert!(saved.contains("Consultorio de prueba"));
    assert!(saved.contains("Ubicación de prueba"));
    assert!(saved.contains("America/Tijuana"));
    assert!(saved.contains("2027-01-15 00:00:30 -08:00"));
  })
  .await;
}
