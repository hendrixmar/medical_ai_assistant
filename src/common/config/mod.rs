pub mod shutdown;
pub mod telemetry;

use serde::Deserialize;
use std::{
  net::{Ipv6Addr, SocketAddr},
  str::FromStr,
  sync::Arc,
};
use tracing::info;

pub type Config = Arc<Configuration>;

#[derive(Deserialize)]
pub struct Configuration {
  /// The environment in which to run the application.
  pub env: Environment,

  /// The address to listen on.
  pub listen_address: SocketAddr,

  /// The port to listen on.
  pub app_port: u16,

  /// The swagger endpoint
  pub swagger_endpoint: String,

  /// The DSN for the database. Currently, only PostgreSQL is supported.
  pub db_dsn: String,

  /// Maximum number of connections in the database pool
  pub db_pool_max_size: u32,

  /// Database connection timeout in seconds
  pub db_timeout: u64,

  /// Whether to run database migrations on startup
  pub db_run_migrations: bool,

  /// Bcrypt hashing cost (default: 12, range: 4-31)
  pub bcrypt_cost: u32,

  pub public_origin: Option<String>,
  pub doctor_totp_key: Option<[u8; 32]>,
  #[serde(default)]
  pub waha_outbound_enabled: bool,
  #[serde(default = "default_contact_cap")]
  pub waha_contact_replies_per_hour: u32,
  #[serde(default)]
  pub claude_api_key: Option<String>,
  #[serde(default)]
  pub claude_model: Option<String>,
  #[cfg(test)]
  #[serde(skip)]
  pub claude_test_endpoint: Option<String>,
  #[serde(default)]
  pub waha_takeover_enabled: bool,
  #[serde(skip)]
  pub waha: Option<crate::modules::whatsapp::config::WahaConfig>,
  #[serde(skip)]
  pub google: Option<crate::modules::google_calendar::config::GoogleConfig>,
}

#[derive(Deserialize, Debug)]
pub enum Environment {
  Development,
  Production,
}

impl Configuration {
  /// Creates a new configuration from environment variables.
  pub fn new() -> Config {
    let env = env_var("APP_ENV")
            .parse::<Environment>()
            .expect("Unable to parse the value of the APP_ENV environment variable. Please make sure it is either \"development\" or \"production\".");

    let app_port = env_var("PORT")
            .parse::<u16>()
            .expect("Unable to parse the value of the PORT environment variable. Please make sure it is a valid unsigned 16-bit integer");

    // Swagger endpoint
    let swagger_endpoint =
      std::env::var("SWAGGER_ENDPOINT").unwrap_or_else(|_| "/docs".to_string());

    let db_dsn = env_var("DATABASE_URL");

    // Default pool size is 10 if not specified
    let db_pool_max_size = std::env::var("DATABASE_POOL_MAX_SIZE")
            .unwrap_or_else(|_| "10".to_string())
            .parse::<u32>()
            .expect("Unable to parse the value of the DATABASE_POOL_MAX_SIZE environment variable. Please make sure it is a valid unsigned 32-bit integer");

    // Default timeout is 5 seconds if not specified
    let db_timeout = std::env::var("DATABASE_TIMEOUT")
            .unwrap_or_else(|_| "5".to_string())
            .parse::<u64>()
            .expect("Unable to parse the value of the DATABASE_TIMEOUT environment variable. Please make sure it is a valid unsigned 64-bit integer");

    // Default to true in development, false in production
    let db_run_migrations = std::env::var("DATABASE_RUN_MIGRATIONS")
            .unwrap_or_else(|_| match env {
                Environment::Development => "true".to_string(),
                Environment::Production => "false".to_string(),
            })
            .parse::<bool>()
            .expect("Unable to parse the value of the DATABASE_RUN_MIGRATIONS environment variable. Please make sure it is a valid boolean");

    // Default bcrypt cost is 12 (valid range: 4-31)
    let bcrypt_cost = std::env::var("BCRYPT_COST")
      .unwrap_or_else(|_| "12".to_string())
      .parse::<u32>()
      .expect("Unable to parse BCRYPT_COST. Please make sure it is a valid integer (4-31)");

    assert!(
      (4..=31).contains(&bcrypt_cost),
      "BCRYPT_COST must be between 4 and 31"
    );
    let public_origin = optional_env("PUBLIC_ORIGIN").map(|value| parse_public_origin(&value).expect("Invalid PUBLIC_ORIGIN: require a canonical HTTPS origin without path, credentials, query or fragment"));
    let doctor_totp_key = optional_env("DOCTOR_TOTP_KEY").map(|value| {
      parse_totp_key(&value).expect("Invalid DOCTOR_TOTP_KEY: require base64-encoded 32 bytes")
    });
    let waha = crate::modules::whatsapp::config::WahaConfig::parse(
      std::env::var("WAHA_BASE_URL").ok().as_deref(),
      std::env::var("WAHA_API_KEY").ok().as_deref(),
      std::env::var("WAHA_WEBHOOK_HMAC_KEY").ok().as_deref(),
    )
    .unwrap_or_else(|message| panic!("{message}"));
    let google = crate::modules::google_calendar::config::GoogleConfig::parse(
      std::env::var("GOOGLE_CLIENT_ID").ok().as_deref(),
      std::env::var("GOOGLE_CLIENT_SECRET").ok().as_deref(),
      std::env::var("GOOGLE_TOKEN_KEY").ok().as_deref(),
      public_origin.as_deref(),
    )
    .unwrap_or_else(|message| panic!("{message}"));
    assert!(
      google
        .as_ref()
        .zip(doctor_totp_key.as_ref())
        .is_none_or(|(google, doctor)| !google.same_key(doctor)),
      "Google token key must be separate from Doctor TOTP key"
    );
    let (waha_outbound_enabled, waha_contact_replies_per_hour) = parse_outbound_policy(
      std::env::var("WAHA_OUTBOUND_ENABLED").ok().as_deref(),
      std::env::var("WAHA_CONTACT_REPLIES_PER_HOUR")
        .ok()
        .as_deref(),
    )
    .unwrap_or_else(|message| panic!("{message}"));
    let (claude_api_key, claude_model) = parse_claude_settings(
      std::env::var("CLAUDE_API_KEY").ok(),
      std::env::var("CLAUDE_MODEL").ok(),
    )
    .unwrap_or_else(|message| panic!("{message}"));
    let waha_takeover_enabled = std::env::var("WAHA_TAKEOVER_ENABLED")
      .ok()
      .map(|value| value.parse::<bool>())
      .transpose()
      .unwrap_or_else(|_| panic!("Invalid WAHA_TAKEOVER_ENABLED: require true or false"))
      .unwrap_or(false);
    assert!(
      !waha_outbound_enabled || (waha.is_some() && public_origin.is_some()),
      "WAHA outbound requires complete WAHA configuration and HTTPS PUBLIC_ORIGIN"
    );
    let bind_address = optional_env("BIND_ADDRESS")
      .map(|value| {
        value
          .parse()
          .expect("Invalid BIND_ADDRESS: require an IP address")
      })
      .unwrap_or(std::net::IpAddr::V6(Ipv6Addr::UNSPECIFIED));
    let listen_address = SocketAddr::from((bind_address, app_port));

    let config = Arc::new(Configuration {
      env,
      listen_address,
      app_port,
      swagger_endpoint,
      db_dsn,
      db_pool_max_size,
      db_timeout,
      db_run_migrations,
      bcrypt_cost,
      public_origin,
      doctor_totp_key,
      claude_api_key,
      claude_model,
      #[cfg(test)]
      claude_test_endpoint: None,
      waha_takeover_enabled,
      waha_outbound_enabled,
      waha_contact_replies_per_hour,
      waha,
      google,
    });

    info!(
      env = ?config.env,
      listen_address = %config.listen_address,
      run_migrations = config.db_run_migrations,
      "Application configuration loaded"
    );

    config
  }

  /// Sets the database DSN.
  /// This method is used in tests to override the database DSN.
  pub fn set_dsn(&mut self, db_dsn: String) {
    self.db_dsn = db_dsn
  }
}

impl FromStr for Environment {
  type Err = String;
  fn from_str(s: &str) -> Result<Self, Self::Err> {
    match s {
      "development" => Ok(Environment::Development),
      "production" => Ok(Environment::Production),
      _ => Err(format!(
        "Invalid environment: {}. Please make sure it is either \"development\" or \"production\".",
        s
      )),
    }
  }
}

fn default_contact_cap() -> u32 {
  20
}

pub fn parse_outbound_policy(
  enabled: Option<&str>,
  cap: Option<&str>,
) -> Result<(bool, u32), &'static str> {
  let enabled = enabled
    .unwrap_or("false")
    .parse::<bool>()
    .map_err(|_| "Invalid WAHA_OUTBOUND_ENABLED: require true or false")?;
  let cap = cap
    .unwrap_or("20")
    .parse::<u32>()
    .ok()
    .filter(|cap| (1..=1000).contains(cap))
    .ok_or("Invalid WAHA_CONTACT_REPLIES_PER_HOUR: require 1..1000")?;
  Ok((enabled, cap))
}
pub fn parse_claude_settings(
  api_key: Option<String>,
  model: Option<String>,
) -> Result<(Option<String>, Option<String>), &'static str> {
  match (api_key, model) {
    (None, None) => Ok((None, None)),
    (Some(key), Some(model))
      if (1..=4096).contains(&key.len())
        && key.bytes().all(|byte| byte.is_ascii_graphic())
        && (1..=256).contains(&model.len())
        && model.bytes().all(|byte| byte.is_ascii_graphic()) =>
    {
      Ok((Some(key), Some(model)))
    }
    (Some(_), Some(_)) => {
      Err("Invalid Claude configuration: API key/model must be non-empty printable values")
    }
    _ => Err(
      "Invalid Claude configuration: CLAUDE_API_KEY and CLAUDE_MODEL must be configured together",
    ),
  }
}

fn optional_env(name: &str) -> Option<String> {
  std::env::var(name).ok().filter(|value| !value.is_empty())
}

pub fn parse_public_origin(value: &str) -> Result<String, &'static str> {
  let url = reqwest::Url::parse(value).map_err(|_| "Invalid PUBLIC_ORIGIN")?;
  if url.scheme() != "https"
    || url.host_str().is_none()
    || !url.username().is_empty()
    || url.password().is_some()
    || url.query().is_some()
    || url.fragment().is_some()
    || url.path() != "/"
    || url.origin().ascii_serialization() != value
  {
    return Err("Invalid PUBLIC_ORIGIN");
  }
  Ok(value.to_owned())
}

pub fn parse_totp_key(value: &str) -> Result<[u8; 32], &'static str> {
  use base64::Engine;
  base64::engine::general_purpose::STANDARD
    .decode(value)
    .map_err(|_| "Invalid DOCTOR_TOTP_KEY")?
    .try_into()
    .map_err(|_| "Invalid DOCTOR_TOTP_KEY")
}

pub fn env_var(name: &str) -> String {
  std::env::var(name)
    .map_err(|e| format!("{}: {}", name, e))
    .expect("Missing environment variable")
}
