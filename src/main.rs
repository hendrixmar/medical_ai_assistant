use server::common::config::shutdown::shutdown_signal;
use server::common::config::telemetry;
use server::common::config::{Configuration, Environment};
use server::database::Db;
use tokio::net::TcpListener;

#[tokio::main]
async fn main() {
  // Loads the .env file located in the environment's current directory or its parents in sequence.
  // .env used only for development, so we discard error in all other cases.
  dotenvy::dotenv().ok();

  // Fixed secret-safe application logging; dependency verbosity is never widened.
  telemetry::setup_tracing();

  // Parse configuration from the environment.
  // This will exit with a help message if something is wrong.
  tracing::debug!("Initializing configuration");
  let cfg = Configuration::new();

  // Initialize db connection.
  tracing::debug!("Initializing db connection");
  let db = Db::new(&cfg)
    .await
    .unwrap_or_else(|_| panic!("Failed to initialize database; check connection configuration"));

  // Run migrations if enabled
  if cfg.db_run_migrations {
    tracing::debug!("Running migrations");
    db.run_migrations()
      .await
      .unwrap_or_else(|_| panic!("Failed to run migrations; a fresh database is required"));
  } else {
    tracing::debug!("Skipping migrations as DATABASE_RUN_MIGRATIONS is disabled");
  }

  // Spin up our server.
  tracing::info!("Starting server on {}", cfg.listen_address);
  let listener = TcpListener::bind(&cfg.listen_address)
    .await
    .expect("Failed to bind address");

  let clock = std::sync::Arc::new(server::modules::doctor::SystemClock);
  let (router, client, google) = server::app::compose(cfg.clone(), db.clone(), clock.clone());
  let (google_db, google_clock, google_cfg) = (db.clone(), clock.clone(), cfg.clone());
  let conversation_google = google.clone();
  let google_worker = tokio::spawn(async move {
    let mut poll = tokio::time::interval(std::time::Duration::from_millis(500));
    poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
      poll.tick().await;
      if server::modules::google_calendar::worker::run_once(
        &google_db,
        google_clock.as_ref(),
        &google_cfg,
        google.as_ref(),
      )
      .await
      .is_err()
      {
        tracing::warn!("Google check unavailable; original cleanup evidence remains durable");
      }
      if server::modules::google_calendar::appointments::run_once(
        &google_db,
        google_clock.as_ref(),
        &google_cfg,
        google.as_ref(),
      )
      .await
      .is_err()
      {
        tracing::warn!("Appointment calendar work remains pending");
      }
    }
  });
  // Independent bounded loops: a 120-second send cannot stall database work.
  let (worker_db, worker_clock, worker_cfg) = (db.clone(), clock.clone(), cfg.clone());
  let worker = tokio::spawn(async move {
    let (db, clock, cfg) = (worker_db, worker_clock, worker_cfg);
    let mut poll = tokio::time::interval(std::time::Duration::from_millis(250));
    poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
      poll.tick().await;
      if server::modules::whatsapp::jobs::run_once(&db, clock.as_ref())
        .await
        .is_err()
      {
        tracing::warn!("Inbox preparation unavailable; pending work will be retried");
      }
      if server::modules::conversation::jobs::run_once(
        &db,
        clock.as_ref(),
        &cfg,
        conversation_google.as_ref(),
      )
      .await
      .is_err()
      {
        tracing::warn!("Conversation storage unavailable; work remains pending");
      }
      if server::modules::conversation::acknowledgements::reconcile(&db, clock.as_ref())
        .await
        .is_err()
      {
        tracing::warn!("Acknowledgement projection unavailable; evidence remains durable");
      }
    }
  });
  let (timer_db, timer_clock, timer_cfg) = (db.clone(), clock.clone(), cfg.clone());
  let appointment_worker = tokio::spawn(async move {
    let mut poll = tokio::time::interval(std::time::Duration::from_millis(250));
    poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
      poll.tick().await;
      if server::modules::appointments::timers::run_once(
        &timer_db,
        timer_clock.as_ref(),
        &timer_cfg,
      )
      .await
      .is_err()
      {
        tracing::warn!("Appointment timers remain pending");
      }
      if server::modules::appointments::retention::run_once(&timer_db, timer_clock.as_ref())
        .await
        .is_err()
      {
        tracing::warn!("Evidence retention work remains pending");
      }
    }
  });
  let (recovery_db, recovery_clock, recovery_cfg, recovery_client) =
    (db.clone(), clock.clone(), cfg.clone(), client.clone());
  let recovery_worker = tokio::spawn(async move {
    let mut poll = tokio::time::interval(std::time::Duration::from_millis(500));
    poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
      poll.tick().await;
      if server::modules::whatsapp::recovery::run_once(
        &recovery_db,
        recovery_clock.as_ref(),
        &recovery_cfg,
        recovery_client.as_ref(),
      )
      .await
      .is_err()
      {
        tracing::warn!("WhatsApp recovery remains pending");
      }
    }
  });
  let sender_cfg = cfg.clone();
  let sender = tokio::spawn(async move {
    let mut poll = tokio::time::interval(std::time::Duration::from_millis(250));
    poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
      poll.tick().await;
      if server::modules::conversation::sender::run_once(
        &db,
        clock.as_ref(),
        &sender_cfg,
        client.as_ref(),
      )
      .await
      .is_err()
      {
        tracing::warn!("Outbox storage unavailable; committed sends will not be retried");
      }
    }
  });

  if matches!(cfg.env, Environment::Development) {
    tracing::info!(
      "Swagger at http://{}{}",
      cfg.listen_address,
      cfg.swagger_endpoint
    );
  }

  axum::serve(listener, router)
    .with_graceful_shutdown(async move {
      shutdown_signal().await;
      // Cancellation drops any in-flight transaction, rolling back its claim and writes.
      let workers = [
        worker,
        sender,
        google_worker,
        appointment_worker,
        recovery_worker,
      ];
      for worker in &workers {
        worker.abort();
      }
      for worker in workers {
        if let Err(error) = worker.await {
          if !error.is_cancelled() {
            tracing::error!("Background worker stopped unexpectedly");
          }
        }
      }
    })
    .await
    .expect("Failed to start server")
}
