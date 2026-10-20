mod authorization;
mod budgets;
mod commit_loss;
pub(crate) mod fixture;
mod maintenance;
mod probes;
mod recovery;
mod refresh;
mod safety;
mod selection;
use crate::{app::tests::with_database, modules::doctor::tests::*};
use base64::Engine;
use sea_orm::{ConnectionTrait, DbBackend, Statement};
use std::sync::Arc;

const SCOPES: &str = "https://www.googleapis.com/auth/calendar.events.owned https://www.googleapis.com/auth/calendar.freebusy https://www.googleapis.com/auth/calendar.calendarlist.readonly";
const ACCESS: &str = "ACCESS_SENTINEL_credential";
const REFRESH: &str = "REFRESH_SENTINEL_credential";

#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL Postgres database"]
async fn oauth_uses_exact_scopes_pkce_live_session_and_encrypted_fresh_pair() {
  with_database(|db| async move {
    let (mut cfg, clock, old_app, totp) = fixture(&db).await;
    drop(old_app);
    let captured = Arc::new(tokio::sync::Mutex::new(Vec::<String>::new()));
    let capture = captured.clone();
    let provider = axum::Router::new().route("/token", axum::routing::post(move |body: String| {
      let capture = capture.clone();
      async move {
        capture.lock().await.push(body);
        axum::Json(serde_json::json!({"access_token":ACCESS,"refresh_token":REFRESH,"expires_in":3600,"token_type":"Bearer","scope":SCOPES}))
      }
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, provider).await.unwrap(); });
    let mut google = super::config::GoogleConfig::parse(Some("client"), Some("CLIENT_SECRET_SENTINEL"), Some(&base64::engine::general_purpose::STANDARD.encode([71;32])), Some("https://practice.test")).unwrap().unwrap();
    google.test_endpoints = Some((format!("{base}/token").parse().unwrap(), format!("{base}/").parse().unwrap()));
    Arc::get_mut(&mut cfg).unwrap().google = Some(google);
    let app = crate::app::router_with_clock(cfg, db.clone(), clock);
    let session = signed_in(&app, &totp, NOW + 30).await;
    let csrf = field(&text(authenticated_get(&app,"/settings",&session).await).await,"csrf_token");
    assert_eq!(post(&app,"/settings/google/connect",None,None,&[]).await.status(),401);
    let response = post(&app,"/settings/google/connect",Some(&session),Some("https://practice.test"),&[("csrf_token",&csrf)]).await;
    assert_eq!(response.status(),303);
    let url = reqwest::Url::parse(response.headers()["location"].to_str().unwrap()).unwrap();
    let params: std::collections::HashMap<_,_> = url.query_pairs().map(|(k,v)|(k.into_owned(),v.into_owned())).collect();
    assert_eq!(url.host_str(),Some("accounts.google.com"));
    assert_eq!(params["scope"],SCOPES);
    assert_eq!(params["prompt"],"consent select_account");
    assert_eq!(params["access_type"],"offline");
    assert_eq!(params["code_challenge_method"],"S256");
    assert_eq!(params["redirect_uri"],"https://practice.test/settings/google/callback");
    assert!(!params.contains_key("include_granted_scopes"));
    assert!(!params.contains_key("login_hint"));
    let state = &params["state"];
    let path = format!("/settings/google/callback?state={state}&code=CODE_SENTINEL");
    let response = authenticated_get(&app,&path,&session).await;
    assert_eq!(response.headers()["location"],"/settings/google");
    assert!(text(response).await.is_empty());
    let requests = captured.lock().await;
    assert_eq!(requests.len(),1);
    let values: std::collections::HashMap<_,_> = reqwest::Url::parse(&format!("https://fixture.invalid/?{}",requests[0])).unwrap().query_pairs().map(|(k,v)|(k.into_owned(),v.into_owned())).collect();
    use sha2::Digest;
    let expected = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(sha2::Sha256::digest(values["code_verifier"].as_bytes()));
    assert_eq!(params["code_challenge"],expected);
    assert!(values["code_verifier"].len()>=43);
    assert_eq!(values["grant_type"],"authorization_code");
    drop(requests);
    let row=db.conn.query_one(Statement::from_string(DbBackend::Postgres,"SELECT status,generation,credential_ciphertext,credential_nonce FROM google_connection WHERE id=1")).await.unwrap().unwrap();
    assert_eq!(row.try_get::<String>("","status").unwrap(),"connected");
    assert_eq!(row.try_get::<i64>("","generation").unwrap(),1);
    let bytes:Vec<u8>=row.try_get("","credential_ciphertext").unwrap();
    assert!(!String::from_utf8_lossy(&bytes).contains("SENTINEL"));
    assert_eq!(row.try_get::<Vec<u8>>("","credential_nonce").unwrap().len(),12);
    authenticated_get(&app,&path,&session).await;
    assert_eq!(captured.lock().await.len(),1,"state is single-use");
    server.abort();
  }).await;
}
