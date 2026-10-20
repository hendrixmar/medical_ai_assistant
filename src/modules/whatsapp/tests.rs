mod intake;
mod jobs;
mod provider;
mod recovery;
mod takeover;

use crate::{app::tests::with_database, modules::doctor::tests::fixture};
use axum::{body::Body, http::Request};
use tower::ServiceExt;

#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
async fn webhook_unconfigured_fails_closed() {
  with_database(|db| async move {
    use crate::modules::doctor::tests::{authenticated_get, get, signed_in, text, NOW};
    let (_, _, app, totp) = fixture(&db).await;
    let session = signed_in(&app, &totp, NOW + 30).await;
    let page = authenticated_get(&app, "/settings/whatsapp", &session).await;
    assert_eq!(page.status(), 200);
    assert!(text(page).await.contains("Conexión sin configurar"));
    assert_eq!(get(&app, "/api/v1/health").await.status(), 200);
    assert_eq!(
      authenticated_get(&app, "/settings", &session)
        .await
        .status(),
      200
    );
    let response = app
      .oneshot(
        Request::builder()
          .method("POST")
          .uri("/api/v1/webhooks/waha")
          .body(Body::from("{}"))
          .unwrap(),
      )
      .await
      .unwrap();
    assert_eq!(response.status(), 503);
  })
  .await;
}
