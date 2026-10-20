use server::common::config::{parse_totp_key, telemetry, Configuration};
use server::database::Db;
use server::modules::doctor::{enrollment, maintenance as doctor_maintenance};
use server::modules::google_calendar::maintenance as google_maintenance;
use std::{env, path::PathBuf, process};

#[derive(Debug, PartialEq)]
enum Command {
  Migrate,
  Bootstrap(PathBuf),
  RotateDoctor,
  RotateGoogle,
  RotateGoogleReconnect,
}

fn command(args: &[String]) -> Option<Command> {
  match args {
    [cmd] if cmd == "migrate" => Some(Command::Migrate),
    [cmd, flag, path]
      if cmd == "bootstrap-doctor" && flag == "--enrollment-file" && !path.is_empty() =>
    {
      Some(Command::Bootstrap(PathBuf::from(path)))
    }
    [cmd] if cmd == "rotate-doctor-key" => Some(Command::RotateDoctor),
    [cmd] if cmd == "rotate-google-key" => Some(Command::RotateGoogle),
    [cmd, flag] if cmd == "rotate-google-key" && flag == "--reconnect-only" => {
      Some(Command::RotateGoogleReconnect)
    }
    _ => None,
  }
}

fn print_usage() {
  eprintln!(
    "Usage: db migrate | db bootstrap-doctor --enrollment-file <new-absolute-private-file> | db rotate-doctor-key | db rotate-google-key [--reconnect-only]"
  );
  eprintln!("Bootstrap requires an already migrated database, DOCTOR_TOTP_KEY and a private 0700 parent directory.");
  eprintln!("Rotation requires the app stopped, the current key in configuration, and the new key only in NEW_DOCTOR_TOTP_KEY / NEW_GOOGLE_TOKEN_KEY.");
}

fn new_key(var: &str) -> [u8; 32] {
  let raw = env::var(var).unwrap_or_default();
  parse_totp_key(&raw).unwrap_or_else(|_| {
    eprintln!("Invalid {var}: require base64-encoded 32 bytes");
    process::exit(1);
  })
}

#[tokio::main]
async fn main() {
  let Some(command) = command(&env::args().skip(1).collect::<Vec<_>>()) else {
    print_usage();
    process::exit(1);
  };
  // Reconnect-only must work when the old Google key is lost: it runs before
  // dotenv/Configuration and reads only the explicitly supplied variables.
  if matches!(command, Command::RotateGoogleReconnect) {
    let Some(dsn) = env::var("DATABASE_URL").ok().filter(|v| !v.is_empty()) else {
      eprintln!("DATABASE_URL is required for rotate-google-key --reconnect-only");
      process::exit(1);
    };
    let new = new_key("NEW_GOOGLE_TOKEN_KEY");
    if let Some(raw) = env::var("DOCTOR_TOTP_KEY").ok().filter(|v| !v.is_empty()) {
      let doctor = parse_totp_key(&raw).unwrap_or_else(|_| {
        eprintln!("Invalid DOCTOR_TOTP_KEY: require base64-encoded 32 bytes");
        process::exit(1);
      });
      if doctor == new {
        eprintln!("NEW_GOOGLE_TOKEN_KEY must be separate from DOCTOR_TOTP_KEY");
        process::exit(1);
      }
    }
    let db = Db::for_maintenance(&dsn).await.unwrap_or_else(|_| {
      eprintln!("maintenance database connection failed");
      process::exit(1);
    });
    match google_maintenance::reconnect_only(&db).await {
      Ok(()) => eprintln!("Google reconnect required. Install the new GOOGLE_TOKEN_KEY before restarting, then reconnect Google and reselect calendars."),
      Err(_) => {
        eprintln!("Google reconnect preparation failed");
        process::exit(1);
      }
    }
    return;
  }
  dotenvy::dotenv().ok();
  telemetry::setup_tracing();
  let cfg = Configuration::new();
  let db = Db::new(&cfg)
    .await
    .unwrap_or_else(|_| panic!("Failed to connect to database; check connection configuration"));
  match command {
    Command::Migrate => {
      db.run_migrations()
        .await
        .unwrap_or_else(|_| panic!("Failed to run migrations; a fresh database is required"));
      tracing::info!("Migrations completed successfully");
    }
    Command::Bootstrap(path) => {
      if let Err(error) = enrollment::bootstrap(&db, &cfg, &path).await {
        eprintln!("{error}");
        process::exit(1);
      }
      eprintln!("Doctor enrollment complete. Wait for a new authenticator code before signing in.");
    }
    Command::RotateDoctor => {
      let new = new_key("NEW_DOCTOR_TOTP_KEY");
      match doctor_maintenance::rotate_key(&db, &cfg, &new).await {
        Ok(retired) => eprintln!("Doctor key rotated; retired {retired} active Doctor Number challenge(s). Install the new DOCTOR_TOTP_KEY everywhere before restarting."),
        Err(_) => {
          eprintln!("Doctor key rotation failed");
          process::exit(1);
        }
      }
    }
    Command::RotateGoogle => {
      let new = new_key("NEW_GOOGLE_TOKEN_KEY");
      match google_maintenance::rotate_key(&db, &cfg, &new).await {
        Ok(count) => eprintln!("Google key rotated; re-encrypted {count} stored credential(s). Install the new GOOGLE_TOKEN_KEY before restarting."),
        Err(_) => {
          eprintln!("Google key rotation failed");
          process::exit(1);
        }
      }
    }
    Command::RotateGoogleReconnect => unreachable!(),
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn only_exact_operator_commands_are_accepted_without_loading_configuration() {
    for args in [
      vec![],
      vec!["seed"],
      vec!["register"],
      vec!["bootstrap-doctor"],
      vec!["bootstrap-doctor", "--password", "secret"],
      vec!["migrate", "extra"],
      vec!["rotate-doctor-key", "extra"],
      vec!["rotate-doctor-key", "--reconnect-only"],
      vec!["rotate-google-key", "--unknown"],
      vec!["rotate-google-key", "--reconnect-only", "extra"],
      vec!["rotate-google-key", "extra"],
    ] {
      assert!(command(&args.into_iter().map(String::from).collect::<Vec<_>>()).is_none());
    }
    assert_eq!(command(&["migrate".into()]), Some(Command::Migrate));
    assert_eq!(
      command(&[
        "bootstrap-doctor".into(),
        "--enrollment-file".into(),
        "/private/enrollment".into()
      ]),
      Some(Command::Bootstrap("/private/enrollment".into()))
    );
    assert_eq!(
      command(&["rotate-doctor-key".into()]),
      Some(Command::RotateDoctor)
    );
    assert_eq!(
      command(&["rotate-google-key".into()]),
      Some(Command::RotateGoogle)
    );
    assert_eq!(
      command(&["rotate-google-key".into(), "--reconnect-only".into()]),
      Some(Command::RotateGoogleReconnect)
    );
  }
}
