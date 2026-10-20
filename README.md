# Medical Assistant

A locally implemented Spanish-language WhatsApp and Doctor-dashboard application
for **one Practice and one Doctor per deployment**. A second Doctor needs a
separate deployment. See [CONTEXT.md](CONTEXT.md) and the [architecture decisions](docs/adr/).

The current local application includes a protected dashboard with attention and
upcoming Appointments; Contact inbox and history; Appointment lists, details and
history; deterministic WhatsApp booking, rescheduling and cancellation; protected
Receipt copying, review and download; Doctor-only Payment Verification; Google
Calendar availability and event/Meet synchronization; reminders; Doctor Number
verification, read-only agenda answers and notifications; and Contact Takeover.
An optional Claude integration only parses closed, read-only preferences: it never
decides availability, payment, or business mutations.

This is **not a clinical record, legally approved service, or production-ready
release**. Patient names belong to one Appointment, not a longitudinal registry;
a Receipt is transfer evidence, never proof money arrived. Only the Doctor can
verify payment. Outbound WhatsApp text and Takeover default OFF. The privacy notice
is provisional. Synthetic/local evidence does not establish live provider
acceptance; real WAHA pairing and storage recovery, Google consent, production
deployment, Doctor enrollment, legal approval and release remain unperformed.

Issue labels #4–#9 below refer to historical implementation/evidence slices, not
the present feature boundary. Their original incidents and provider gates remain
historical evidence; later local integration does not erase them.

## Development configuration

Prerequisites: Rust 1.88 or newer, Postgres, and Docker Compose v2 for the optional test
fixture. Run commands from the repository root.

Copy the sample yourself, then edit it before starting the application:

```sh
cp .env.sample .env
```

Set `DATABASE_URL` to a **separate, fresh development database** with your own
credentials. The sample intentionally provides none. A connection string has the
shape `postgres://<user>:<url-encoded-password>@<host>:<port>/<fresh_database>`.
Never commit `.env` or credentials.

| Variable | Sample / runtime default | Purpose |
| --- | --- | --- |
| `APP_ENV` | `development` | `development` or `production` |
| `PORT` | `8080` | Application listener port |
| `BIND_ADDRESS` | sample `127.0.0.1`; absent binds `[::]` | Bind address; use a private HTTPS proxy for browser access |
| `SWAGGER_ENDPOINT` | `/docs` | Development-only Swagger UI path |
| `DATABASE_URL` | empty; required | Fresh development Postgres database |
| `DATABASE_POOL_MAX_SIZE` | sample `50`; runtime `10`; Compose `20` | Maximum pooled connections |
| `DATABASE_TIMEOUT` | `5` seconds | Database connection timeout |
| `DATABASE_RUN_MIGRATIONS` | sample `true`; runtime development `true`, production `false` | Whether server startup runs migrations; Compose explicitly sets `true` |
| `BCRYPT_COST` | `12` | Password-hashing cost, 4–31; retain 12 or stronger outside isolated tests |
| `PUBLIC_ORIGIN` | absent | Exact canonical HTTPS browser origin; required for Doctor login and provider OAuth/outbound |
| `DOCTOR_TOTP_KEY` | absent | Stable base64-encoded 32-byte key for TOTP and purpose-separated Doctor Number encryption/fingerprints |
| `WAHA_BASE_URL` | absent | Optional canonical WAHA origin; HTTPS except explicit loopback HTTP |
| `WAHA_API_KEY` | absent | Plain server-only API credential; no default |
| `WAHA_WEBHOOK_HMAC_KEY` | absent | Operator-provisioned literal UTF-8 secret of at least 32 characters |
| `WAHA_OUTBOUND_ENABLED` | `false` | Independent opt-in for generated ID/seen/typing/text calls; intake and queues still operate while off |
| `WAHA_TAKEOVER_ENABLED` | `false` | Independent opt-in for Practice-phone Takeover attribution; leave off until live WAHA peer-attribution evidence is accepted |
| `WAHA_CONTACT_REPLIES_PER_HOUR` | `20` | Application rolling-hour cap per Contact, 1–1000; not a provider safe threshold |
| `GOOGLE_CLIENT_ID` | absent | Optional Google Web application OAuth client ID |
| `GOOGLE_CLIENT_SECRET` | absent | Server-only OAuth client secret |
| `GOOGLE_TOKEN_KEY` | absent | Stable standard-base64 32-byte Google-only AES-256-GCM key, distinct from `DOCTOR_TOTP_KEY` |
| `CLAUDE_API_KEY` | absent | Optional Claude API key; requires explicit `CLAUDE_MODEL` |
| `CLAUDE_MODEL` | absent; no default | Optional model selection; requires `CLAUDE_API_KEY` |
| `RUST_LOG` | ignored | Cannot widen fixed `off,server=info` logging |

For the application configuration, WAHA values are either all absent or all
supplied and valid; supplied empty, partial or malformed values fail startup.
Google's three values follow the same absent-or-complete rule and require canonical
HTTPS `PUBLIC_ORIGIN`. Claude's key and explicit model are an optional nonempty
pair, with no fallback model. Never add empty `KEY=` entries as a substitute for
omitting an optional setting. No provider endpoint override is available.

The removed historical account migration is unsupported. Use a **fresh database**;
unknown or old migration history fails closed. Preserve an old database separately;
this is not an upgrade path, reset command, or credential seed. Eleven ordered
migrations currently compose the local schema:

1. `m202610070001_doctor_access`: the singleton Doctor, hashed-token sessions,
   login challenges and persistent login-limit state; enrollment remains an
   operator-only CLI action.
2. `m202610070002_practice_configuration`: one Practice and UUID-keyed Services.
   Only the approved 30-minute grid, 30-day horizon, 180-minute review margin and
   10-minute grace defaults are seeded; other choices begin unset.
3. `m202610140001_whatsapp_inbox`: authenticated inbox evidence and the original
   `prepare_inbox` processing job.
4. `m202610140002_conversation`: Contacts, evidence-backed aliases, Followup
   Requests, outbox, signed acknowledgement evidence and typed jobs.
5. `m202610140003_doctor_number`: protected Doctor Number binding, current
   challenge and unique keyed challenge history.
6. `m202610140004_google_calendar`: encrypted Google connection state, OAuth
   attempts, explicit calendar choices and probe/cleanup evidence.
7. `m202610160001_appointments`: Appointment snapshots, Holds, append-only
   history, attention items, calendar/payment/reminder jobs, and the
   `reminder_2h_enabled=false`, `review_interval_minutes=240` and
   `urgent_notice_minutes=120` defaults.
8. `m202610160002_receipts`: copied Receipt evidence, intake state and candidate
   links; evidence is not automatic Payment Verification.
9. `m202610160003_provider_recovery`: durable WAHA recovery checkpoints/targets,
   identity evidence and Contact Takeover state.
10. `m202610160004_application_outbox`: shared message purposes and text-retention
   markers, plus durable processing claim fences.
11. `m202610200001_doctor_number_fingerprint_key`: nullable encrypted stable
   Doctor Number fingerprint subkey on the singleton state (both columns NULL
   together, or 48-byte ciphertext + 12-byte nonce); existing rows stay NULL/NULL
   until the first offline Doctor key rotation. Its down operation refuses
   destructive removal.

`processing_jobs` has exactly three constrained kinds:
`prepare_inbox`, `process_conversation`, `send_outbox`. The separate
`appointment_jobs` table has `calendar_sync`, `calendar_cancel`, `reminders`,
`review`, `expiry` and `conflict`. No migration rollback may discard Doctor,
Practice, Appointment, Receipt, recovery or messaging evidence. Migrations are
idempotently applied by SeaORM; re-running them preserves saved records.

For a fresh local development database, run migrations explicitly if desired and
start the server:

```sh
cargo run --locked --bin db -- migrate
cargo run --locked --bin server
```

The `db` CLI accepts only `migrate`, `bootstrap-doctor` with a new private
enrollment file, `rotate-doctor-key`, and `rotate-google-key` (with optional
`--reconnect-only`). There are no seeded accounts, reset/upsert operations or public
registration routes. `DATABASE_RUN_MIGRATIONS` defaults to `true` in development
and `false` in production; `deployment/compose.yml` explicitly enables them.
Unknown legacy migration history fails startup rather than being rewritten.

- `GET /api/v1/health` is a static health response, not a database-readiness check.
- Development exposes Swagger at `SWAGGER_ENDPOINT` (default `/docs`); production
  returns `404` for Swagger UI and OpenAPI.
- Public browser routes are `GET/POST /login`, `GET /aviso-de-privacidad` and
  `GET /assets/doctor.css`. `GET/POST /login` uses pre-authentication CSRF and
  exact-Origin checks; it is not a registration flow.
- The Doctor-protected GET routes include `/dashboard`, `/settings`,
  `/settings/whatsapp`, `/settings/whatsapp/qr`, `/settings/doctor-number`,
  `/settings/google`, `/contacts`, `/contacts/{id}`, `/appointments`,
  `/appointments/{id}` and `/dashboard/payments/receipts/{id}` plus its `/file` route.
- Protected POST routes include `/logout`; `/settings/{details,services,schedule,payments}`;
  `/settings/whatsapp/start`; `/settings/whatsapp/request-code`; `/settings/doctor-number/challenge`;
  `/settings/google/{connect,calendars,probe}` and
  `/settings/google/probes/{id}/cleanup`; `/contacts/{id}/takeover/end`;
  `/followups/{id}/resolve`; `/appointments/{id}/{cancel,reschedule}` and
  `/appointments/{id}/refund/resolve`; and
  `/dashboard/payments/receipts/{id}/{association,verify,reject,late-outcome,cancel}`.
  All share the Doctor session, exact Origin and CSRF boundary.
- `GET /settings/google/callback` also verifies the initiating session and OAuth
  state; its outer response is a clean, resource-free, no-store/no-referrer redirect.
- Public `POST /api/v1/webhooks/waha` uses bounded raw-body HMAC-SHA512
  authentication, not browser cookies/CSRF. Durable inbox or signed acknowledgement
  evidence commits before `200`.
- Missing origin/key or Doctor enrollment makes login unavailable (`503`) while
  health remains available. Malformed nonempty configuration fails startup with a
  value-free diagnostic. Bank details are authenticated-only and absent from health
  and OpenAPI.

**Remote browser previews:** a localhost URL is not reachable from another
machine. The app and WAHA host ports in `deployment/compose.yml` bind to loopback
only (`APP_PORT`, default `18080`; `WAHA_PORT`, default `13000`); the database
has no published host port. Configure a tailnet-private HTTPS proxy such as
Tailscale Serve to forward the Doctor UI to `http://127.0.0.1:18080`, preserve
existing Serve routes, and do not use Funnel or public exposure. `PUBLIC_ORIGIN`
must be the exact HTTPS origin the Doctor will use. This guide does not deploy or
publish a browser preview.

## Operator runbook (manual; not executed here)

These commands describe the checked-in deployment interface, not permission or
evidence to deploy. Obtain operator authorization, counsel/privacy approval,
provider acceptance and recovery decisions first. Never use production patient or
Contact data for rehearsal. Run from the repository root; keep shell tracing off
and keep all secret files outside the repository.

### Prepare the Compose environment and image

Create an owner-only (`0600`) Compose interpolation file and a separate owner-only
app provider environment file. The Compose file needs the values named by
`deployment/compose.yml`: `POSTGRES_ADMIN_USER/PASSWORD`, `APP_DB_NAME`,
`APP_DB_USER/PASSWORD`, `WAHA_DB_USER/PASSWORD`, `WAHA_API_KEY`,
`WAHA_DASHBOARD_USERNAME/PASSWORD`, `WAHA_WEBHOOK_HMAC_KEY`, `PUBLIC_ORIGIN`,
`DOCTOR_TOTP_KEY` and `APP_PROVIDER_ENV_FILE`. Use distinct database-role names.
The three database passwords must be different URL-safe ASCII values of at least
32 characters; `deployment/init-postgres.sh` rejects reuse and unsafe characters.
Generate each independently with a private secret manager (the script's documented
generator is `openssl rand -hex 32`); do not print or reuse generated values.
Postgres initialization only creates these roles on an empty data volume; changing
the file later does not rotate existing database passwords.

`APP_PROVIDER_ENV_FILE` is a **required Compose path**, not a provider credential:
the file must exist, and the path should be absolute and outside the repository.
It may be a zero-byte file when no optional Google or Claude provider is configured.
That is different from putting an empty `GOOGLE_*` or `CLAUDE_*` key inside it:
optional keys must be absent or supplied as their complete nonempty set/pair.
Keep `DOCTOR_TOTP_KEY` stable and escrow it separately. If Google is enabled,
`GOOGLE_TOKEN_KEY` is also stable and separately escrowed; it must differ from the
Doctor key. `WAHA_OUTBOUND_ENABLED=false` and `WAHA_TAKEOVER_ENABLED=false` are
the explicit safe defaults; neither follows from pairing or saving settings.

The Compose environment file must be owner-only and private. Set its path in the
operator shell without placing credentials in command history:

```sh
set +x
export COMPOSE_ENV_FILE=/etc/medical-assistant/compose.env
compose() {
  docker compose --env-file "$COMPOSE_ENV_FILE" \
    --project-name medical-assistant-vps \
    --file deployment/compose.yml "$@"
}
compose config --quiet
compose build app
compose up -d --wait db waha
compose run --rm --no-deps --entrypoint /app/db app migrate
```

The Compose stack uses separate application and WAHA database roles; the
application role owns only its dedicated `APP_DB_NAME`. WAHA's PostgreSQL
session/media URLs use a separate role/DSN against the cluster's `postgres`
database, not the application `DATABASE_URL`. Its host API/dashboard port is
loopback-only. Review an existing filesystem-backed `default` GOWS session and
authorize a migration/rollback plan before switching storage; the app does not
migrate or re-pair that state for you. The app shares `waha`'s network namespace,
so this runbook starts `waha` before its one-off CLI command, without pairing the
Practice session.

### Enroll the sole Doctor, then start the private service

Run enrollment only in an operator-controlled terminal, after `migrate` succeeds.
The container image's normal entrypoint is `/app/server`; override it with the
actual CLI binary `/app/db`. The mounted host directory is private and owned by the
same UID used for the CLI, so its temporary `0600` URI file can be imported locally:

```sh
ENROLLMENT_DIR="$HOME/.config/medical-assistant/enrollment"
install -d -m 700 "$ENROLLMENT_DIR"
compose run --rm --no-deps --user "$(id -u):$(id -g)" \
  --volume "$ENROLLMENT_DIR:/enrollment" \
  --entrypoint /app/db app bootstrap-doctor \
  --enrollment-file /enrollment/doctor-uri
```

Use the hidden prompts for a unique password of at least 12 characters and no more
than 72 UTF-8 bytes, followed by the current six-digit TOTP. `ENROLLMENT_DIR` must
be a new operator-owned mode-`0700` directory with no symlink components; the CLI
writes to the new absolute `/enrollment/doctor-uri` path. Import the temporary URI
only through a trusted local authenticator method, never a QR website, shell
argument, chat or log. Enrollment writes nothing until the code is proven; wait
for the next authenticator code before login because confirmation consumes it.
The artifact is removed on success and ordinary errors; inspect and securely
remove a leftover after crash or power loss. Keep the stable key and fingerprint
history in the recovery plan; offline Doctor key rotation is maintenance (see
below), and a lost key without its matching escrow/backup remains unrecoverable.

Configure the approved tailnet-private HTTPS proxy to loopback and confirm
`PUBLIC_ORIGIN` exactly matches its HTTPS origin. Then start the local stack:

```sh
compose up -d --wait app
```

Compose starts the declared WAHA dependency; app startup runs migrations as
configured. The app listens inside WAHA's network namespace, while the published
Doctor UI and WAHA API/dashboard ports remain bound to host loopback. Do not
publish Postgres, WAHA or the app directly to the public Internet.

### Manually authorize providers

1. **WAHA:** Before use, an authorized operator provisions `WAHA_API_KEY`,
   dashboard credentials and a distinct random `WAHA_WEBHOOK_HMAC_KEY`. In the
   operator-only WAHA configuration, register the exact
   `PUBLIC_ORIGIN` + `/api/v1/webhooks/waha` callback and matching HMAC secret;
   the application never configures webhooks. Preserve other hooks/settings. Open
   the protected **Configuración → Conexiones → WhatsApp** page, start the
   existing `default` GOWS session and have the Doctor scan its protected QR using
   the Practice phone. Confirm reported `WORKING` state and the resolved Practice
   Number; a QR or start acknowledgement alone is not pairing proof. Do not claim
   that synthetic tests validate a real callback or PostgreSQL session restore.
2. **Doctor Number:** Only after an authorized working Practice session, open
   **Configuración → Conexiones → Número del doctor**, generate the native
   six-digit challenge and have the Doctor send only those digits from their own
   phone to the Practice Number. Verification requires current signed sender and
   numeric `me` evidence; a typed number, historical alias or media caption is
   not proof. This supports read-only agenda answers and notifications, not
   business mutations. Keep Takeover disabled until version-specific WAHA phone
   event attribution is independently accepted.
3. **Google Calendar:** If and only if provisioned, populate all three optional
   Google variables in the app-only provider file. Register the exact OAuth
   redirect URI `PUBLIC_ORIGIN/settings/google/callback`. The Doctor initiates
   consent from **Configuración → Conexiones → Google Calendar**, explicitly
   selects the Booking Calendar and any Blocking Calendars, then runs the
   capability check. The app requests only the approved scopes. A test or saved
   credential is not live Google acceptance; preserve cleanup evidence and the
   independent Google encryption key.
4. **Messaging/legal gate:** Text dispatch and Takeover remain OFF by default.
   Changing `WAHA_OUTBOUND_ENABLED` or `WAHA_TAKEOVER_ENABLED` is a separate
   operator release decision, requires consistent restarts and approved live
   evidence; it is not part of pairing. The privacy notice remains provisional,
   and legal, consent and ordinary Contact reachability approval are still needed.

### Encrypted off-server backup and opt-in nightly timer

`deployment/backup.sh` requires `age`, Docker, `sftp` and `sha256sum`. It dumps the
full PostgreSQL cluster (including WAHA storage; omits role password hashes),
encrypts to the public `AGE_RECIPIENT`, writes a SHA-256 sidecar, and uploads both
through batch SFTP with `StrictHostKeyChecking=yes`. The SFTP host key and
noninteractive SSH key must already be provisioned; `BACKUP_SSH_TARGET` is
`user@host` (no port/path), `BACKUP_REMOTE_DIR` is an existing absolute directory
with only letters, digits, `_` and `-` path components, and the target must be a
separate off-server system. Set `OFFSITE_BACKUP_ACK=true` only after verifying
that separation. The age private identity stays off this VPS. The script does not
choose retention or prune remote backups.

For the optional user-level systemd units, create the private
`$HOME/.config/medical-assistant/backup.env` and populate it privately with
`MEDICAL_ASSISTANT_ROOT`, `COMPOSE_ENV_FILE`, `BACKUP_SSH_TARGET`,
`BACKUP_REMOTE_DIR`, `AGE_RECIPIENT` and `OFFSITE_BACKUP_ACK=true`.
`COMPOSE_ENV_FILE` must be an absolute regular non-symlink file with mode `0600`
or `0400`. The service uses that exact environment file and invokes the checked-in
backup script:

```sh
install -d -m 700 "$HOME/.config/medical-assistant" \
  "$HOME/.config/systemd/user"
cp -n deployment/medical-assistant-backup.service \
  "$HOME/.config/systemd/user/"
cp -n deployment/medical-assistant-backup.timer \
  "$HOME/.config/systemd/user/"
systemctl --user daemon-reload
```

After setting up and reviewing the separate SSH host-key trust, key custody,
offsite target, retention policy and `backup.env`, one may run a backup manually
with `systemctl --user start medical-assistant-backup.service`. The nightly timer
is an explicit opt-in, scheduled for 03:00 UTC; it has **not** been installed,
enabled or started by this work:

```sh
systemctl --user enable --now medical-assistant-backup.timer
systemctl --user list-timers medical-assistant-backup.timer
```

Keep the age identity, stable `DOCTOR_TOTP_KEY`, and (if used)
`GOOGLE_TOKEN_KEY` in separately controlled escrow, not alongside the encrypted
database dump. A restored database without the matching app keys cannot recover
TOTP, Doctor Number or Google credential use.

### Offline maintenance key rotation (operator-only)

Rotation is maintenance, **not compromise remediation**: the authenticator
secret and existing login sessions do not change, and someone holding an old
database plus its old key may still know the TOTP secret. It never revokes a
Google provider grant. Lost Doctor key remains unrecoverable without its
matching escrow/backup; there is no reset, re-enrollment-on-existing-data or
backdoor (fresh-database re-enrollment loses Practice data).

Doctor rotation preserves lifetime Doctor Number code-value non-reuse: the
stable fingerprint subkey is stored only encrypted in the database
(`doctor_number_state` envelope, migration 11) and is re-wrapped unchanged,
together with the same TOTP secret, in one offline transaction; active
challenges are retired. Google rotation re-encrypts stored credentials under
the new key and supersedes pending OAuth verifiers, leaving
generation/selections/targets/Appointments untouched. Google
`--reconnect-only` needs no old Google key: it discards local tokens, marks
the connection `reauthorize`, and the Doctor reconnects through the ordinary
authenticated flow (calendar reselection is required). Submitted-code inbox
residue is unaffected by rotation.

Procedure: stop/quiesce **all** application/worker instances; verify a matching
current backup and key custody; apply the additive migration explicitly; run
maintenance with the new key injected only for the CLI (values from a private
0600 file/environment, never argv, chat or logs); install the new current key
in the private deployment env; unset `NEW_*`; restart and verify:

```sh
compose run --rm --no-deps -e NEW_DOCTOR_TOTP_KEY --entrypoint /app/db app rotate-doctor-key
compose run --rm --no-deps -e NEW_GOOGLE_TOKEN_KEY --entrypoint /app/db app rotate-google-key
compose run --rm --no-deps -e NEW_GOOGLE_TOKEN_KEY --entrypoint /app/db app rotate-google-key --reconnect-only
```

Record backup/key epochs **without secret values**: retain each retired key in
separately controlled escrow for as long as any retained backup/WAL needs it —
a fresh backup alone does not make retained old backups decryptable with the
new key. Do not destroy keys or choose/prune backup retention here. Require an
isolated post-rotation restore rehearsal before calling the new recovery path
verified. Real rotation, key provisioning and backup operations are
operator-only; automated proof uses only synthetic credentials/databases.

### Isolated, offline restore

Download a selected encrypted dump **and its adjacent `.sha256` sidecar** to an
isolated recovery host using the same pinned-host-key SFTP policy. Prepare an
owner-only restore environment file containing only
`RESTORE_POSTGRES_USER` (a bootstrap name not present in the source cluster),
`RESTORE_POSTGRES_PASSWORD` (temporary strong password) and
`SOURCE_APP_DB_NAME`. Export `BACKUP_FILE`, `AGE_IDENTITY_FILE` and
`RESTORE_ENV_FILE` as absolute paths; the identity and env files must be regular
non-symlink files with mode `0600` or `0400`. Set the isolation acknowledgement
only after confirming this is a separate recovery environment:

```sh
export RESTORE_ISOLATION_ACK=true
bash deployment/restore-isolated.sh
```

The script verifies the sidecar and age authentication before restoring. It starts
only `restore-db` from `deployment/restore.compose.yml`, on an internal network
with tmpfs storage and no published ports; it contains no application, WAHA,
worker, provider credentials or message consumer. It also verifies the
application's migration table and prints the generated restore Compose project
name. On success the isolated DB stays running for inspection. After review, set
`RESTORE_PROJECT_NAME` to the exact printed value and remove only that project:

```sh
export RESTORE_PROJECT_NAME
docker compose --env-file "$RESTORE_ENV_FILE" \
  --project-name "$RESTORE_PROJECT_NAME" \
  --file deployment/restore.compose.yml down
```

Never attach an app or provider to it or restore over production. The separate
age/key escrow is required for recovery.

The commands in this runbook are examples of the real checked-in flags and
interfaces; this documentation update did not execute deployment, pairing,
enrollment, provider consent, backup, restore or timer activation.

### Authentication behavior

Sessions use random 32-byte opaque cookies, with only SHA-256 token hashes in
Postgres and independent random CSRF values. The `__Host-doctor_session` cookie is
Secure, HttpOnly, SameSite=Lax, Path=/, and lasts at most 30 days without sliding
renewal. Logout requires a valid session, exact `Origin`, and its form CSRF token;
it revokes the persisted session and expires the cookie. Future sensitive routes
must join the same protected router, not implement their own authorization.
Notification links, phone numbers, and WhatsApp identities never authorize access.

Login itself requires an exact `Origin` and a matching pre-authentication cookie
and form token. These challenges last 10 minutes; issuance is capped at 20 per
5-minute window, with expired-row cleanup and at most 60 outstanding unexpired
challenges across overlapping issuance windows under ordinary monotonic time. The single deployment-wide attempt
bucket permits 5 attempts per fixed 5-minute window **before bcrypt**, counting
failures of either factor; rejected attempts do not extend it. Limits persist
across restarts and do not trust forwarded IP headers. This intentionally means
repeated requests can temporarily deny even the real Doctor a login attempt.

A TOTP match uses a 30-second step and one-step clock skew; its actual matched
counter is consumed transactionally with session creation, preventing replay,
including simultaneous submissions. Password work uses bounded blocking workers.
TOTP secrets are AES-256-GCM encrypted with fresh OS-random nonces and purpose-bound
associated data. SQL bind logging is disabled; do not add request-body, cookie,
CSRF, password, or enrollment-URI logging in a proxy or application middleware.

Invalid credentials return generic `401` views, CSRF failures `403`, limits `429`
with `Retry-After`, and unavailable configuration/storage `503`. Successful form
submissions redirect with `303`; `return_to` allows `/dashboard`, `/settings`,
`/settings/whatsapp`, `/settings/doctor-number`, `/settings/google`, `/appointments`,
`/contacts`, and UUID-only `/appointments/{id}`, `/contacts/{id}` and
`/dashboard/payments/receipts/{id}` paths.
Authentication pages use `no-store`, a restrictive CSP (no scripts), frame denial,
and `nosniff`. `Referrer-Policy: same-origin` withholds cross-origin referrers while
preserving the `Origin` on native same-origin form submissions. Do not replace it
with `no-referrer`: browsers can then send `Origin: null`, correctly rejected by
our CSRF checks, making login and logout unusable. Global API CORS does not grant credentialed cross-origin access and
cannot replace the mandatory origin/token checks.

## Practice configuration

After operator enrollment and login, use **Configuración** (`/settings`). The page
opens the first incomplete step, or details when all four steps are complete.
Step links let the Doctor revisit saved decisions. Each POST validates the entire
submitted step before a single atomic SQL update/upsert; it never overwrites other
steps. Invalid submissions return `422` with Spanish field errors and the posted
values, without partially changing stored configuration. Successful writes use
`303` Post/Redirect/Get. Storage failures return `503`, never a success notice.

1. **Datos del consultorio:** optional name (120 characters), required location
   (500 characters), and an explicitly selected IANA timezone. There is no inferred
   or preselected timezone. `chrono-tz` supplies the IANA rules and DST conversion
   absent from `chrono` alone; the example uses the application clock and names the
   selected timezone and UTC offset. It is not an Appointment.
2. **Servicios:** a name (120 characters), positive whole-minute duration, and an
   exact nonnegative MXN amount. Prices use integer centavos, never floating point;
   accept a decimal point and at most two fractional digits, not commas, exponent
   notation, negatives, or overflow. Maximum representable price is
   `92233720368547758.07` MXN. A new form supplies a UUIDv7; repeat submissions with
   that UUID update the same Service, and edits preserve it. No deletion or retirement
   behavior is implemented.
3. **Horarios:** choose open or closed for each of seven days. Open days accept
   multiple nonoverlapping `HH:MM-HH:MM` ranges separated by commas; `24:00` is allowed
   only as the end. Split overnight hours between two days. Closed days have no
   ranges; an unconfigured day keeps the step incomplete. All seven explicitly
   closed days are valid. Buffers and minimum notice require explicit nonnegative
   whole minutes; grid and horizon must be positive. Initial defaults are a
   **30-minute grid** and **30-day horizon**. Cancellation policy is Doctor-supplied
   text (up to 4,000 characters), never fabricated. A deliberately restrictive
   minimum notice is not rejected just because it exceeds the horizon.
4. **Pagos de video:** bank (120 characters), beneficiary (200 characters), and
   an 18-ASCII-digit CLABE stored as text, preserving leading zeros. This is a format
   check, not checksum, account ownership, or payment verification. The Receipt
   Deadline window requires an explicit positive whole-minute choice; review margin
   defaults to **180 minutes** and grace to **10 minutes**, both editable and
   nonnegative. Grace covers transit delay, **not an extension of the send-by deadline**.

All interval fields use integers up to `2147483647`; validation reports this
technical bound rather than choosing an operational policy. These values are not
a real Practice's configuration. Completion is derived from valid saved fields and
Services, not a posted completion flag. The Conexiones footer links to actual WAHA
connection status, Doctor Number verification and Google Calendar connection/capability checks.
WhatsApp live-provider acceptance is separate from configuration completion. Saving
settings changes Practice configuration only; it does not create an Appointment or
calculate an individual time's availability/deadline. Appointment and video
workflows are implemented separately. Creation snapshots the saved Service,
schedule and payment instructions so later edits cannot rewrite an existing offer.
Practice routes are merged **inside** the existing Doctor protected router. They
reuse its opaque session, exact Origin and CSRF checks, stylesheet, security headers,
`Referrer-Policy: same-origin`, and native forms. Protected form bodies are bounded
to 64 KiB to accommodate URL-encoded Unicode policy text; public login remains
bounded to 8 KiB. Bank details are authenticated-only and absent from health and
OpenAPI responses. No new CLI configuration or environment variables are needed.

## Current local application behavior and integration boundaries

The #6–#9 sections below retain historical evidence and safety boundaries. Earlier
statements that Appointment journeys, agenda answers or notifications were
unavailable describe their old slice only; the current local application has
integrated them. The linked records preserve primary-source research, test
evidence and live acceptance gates. Neither synthetic tests nor local browser
rehearsal establishes a working production provider.

### Appointment, Contact and payment workflow

The Doctor configures one Practice, Service snapshots, IANA timezone, schedule,
buffers, payment instructions and review policy. Contacts can book or request
rescheduling/cancellation through deterministic WhatsApp flows; the Doctor sees
the Contact inbox/history and Appointment list/detail/history on the protected
dashboard. A Contact may have at most three future open Appointments. A Patient's
name is stored on that Appointment only, not in a longitudinal registry.

Appointments use `requested | awaiting_payment | confirmed | cancelled | expired`;
Payment uses `not_required | awaiting_receipt | under_review | verified |
rejected`; Calendar synchronization uses `not_yet | pending | synced | failed`.
Stable Appointment IDs, revision fences and overlapping Holds protect concurrent
booking/rescheduling. Service name, duration, price, timezone, location, bank
instructions and buffers are snapshots; changes to current Practice settings do
not rewrite an existing Appointment.

Video begins as `awaiting_payment` / `awaiting_receipt`. The Contact receives
snapshotted transfer instructions and a Receipt Deadline. A Receipt is copied as
evidence and can be reviewed or downloaded only through the protected Doctor
dashboard. A timely Receipt must also arrive before Hold expiry to move the
Appointment to `under_review` and protect its Hold while the Doctor reviews it.
The Receipt never verifies money: only the authenticated Doctor's explicit
Payment Verification marks Payment `verified`. Rejection, replacement Receipt,
late Receipt, clarification and Doctor disposition remain explicit history and
attention decisions. A pending-payment reschedule keeps the same Appointment and
its original Receipt Deadline; it does not restart that window. A Receipt arriving
after the Hold expires stays late, even if WhatsApp says it was sent before the
deadline; it never silently restores the old slot.

Google availability or Calendar failures remain visible as pending/failed state;
there is no fabricated Meet URL. A video appointment reminder includes a Meet link
only after actual Google conference success, Calendar synchronization and verified
Payment. Cancellation after verified payment creates a `refund_due` Attention
Item. The Doctor must handle the refund outside the application, then use
**Marcar reembolso como resuelto**; the idempotent action records history and
resolves attention once, but never transfers funds or reverses verified Payment.

The system schedules a 24-hour Reminder for eligible Appointments and an optional
2-hour Reminder when the Doctor enables it. The app records an explicit attendance
reply separately; a send/acknowledgement/read status is not attendance. Contact
messages, payment details, status, reminders and Doctor notifications share an
outbox with Appointment-revision/Doctor binding and retention/Takeover fences.
Outbound text remains OFF by default. When Takeover is enabled, conversational
replies are suppressed for that Contact; essential operational notices remain eligible.

The verified Doctor Number can ask the read-only agenda for stored Appointment
facts in the Practice timezone. It cannot change records or invent availability.
Optional Claude parsing maps preferences to a closed read-only representation;
application code owns all facts, validation, availability, payment decisions and
mutations.

Retention removes eligible Receipt bytes and inbox/outbox text at the 90-day
boundary only after every associated Appointment is terminal and the association
graph is rechecked. Structured facts and Appointment History remain retained for
at least five years. There is no automatic destructive purge after five years
while the legal anchor and disposition remain unresolved. Unassociated evidence
gets Doctor attention, not an invented deletion deadline. This behavior is not a
legal-compliance certification.

### WAHA / GOWS (#6 historical slice; live acceptance still open)

See the [pinned capability and operations record](docs/integrations/waha.md).
The application reads reported status/QR and starts the existing `default` GOWS
session only on a protected, Origin/CSRF-checked Doctor POST. It does not configure
webhooks, create/stop/delete sessions or download Receipt bytes from WAHA.
Authenticated incoming webhooks use bounded raw-body HMAC-SHA512 authentication,
deduplicate by provider message ID, preserve original identity/timestamps/evidence,
and enqueue durable preparation. `processing_jobs` are polled with short
Postgres transactions and `FOR UPDATE SKIP LOCKED`; bounded retries and terminal
diagnostics preserve failed work. Prepared input is not a claim that an outgoing
reply was sent.

The Compose configuration targets WAHA `2026.9.2`, one GOWS session and separate
Postgres session/media storage with a WAHA role. Live WAHA runtime acceptance,
pairing, authorized transition from any existing filesystem-backed session, real
webhook delivery, storage persistence across container recreation and incoming-call
evidence remain operator gates. WAHA may lose its own in-memory webhook retries
on restart; application durability cannot remove that upstream gap.
See the [WAHA storage/cutover checklist](docs/integrations/waha.md).

### Contact conversations and outbox (#7 historical slice; current integrated flow)

The [outbox record](docs/integrations/conversation-outbox.md) and [pinned transport
research](docs/integrations/waha-messaging.md) retain the earlier slice's exact
dispatch evidence. The former menu's explicit "not implemented" responses are
superseded: the current app supports deterministic booking, rescheduling,
cancellation, Followup Requests and Appointment/payment workflows. Followup
resolution is an authenticated Doctor action and enqueues one logical notification;
it does not claim personal contact occurred.

`WAHA_OUTBOUND_ENABLED=false` suppresses generated ID/seen/typing/send calls while
intake, queueing and acknowledgement evidence continue. Pairing, a `WORKING` state
or saving settings never enables text dispatch. When explicitly enabled, the
application commits uncertainty and budget before one `sendText` call; a committed
item is not automatically POSTed a second time, even after an ambiguous outcome.
Positive signed acknowledgements reconcile monotonically, but do not prove
recipient delivery, ordering or exactly-once effects. The per-Contact dispatch
window is an application fence, not provider-side cancellation or a delivery
guarantee. Keep the provider-specific uncertainty and live launch gates in the
linked record.

The public privacy notice remains **PROVISIONAL**. Legal/consent approval and
ordinary Contact reachability of that notice are release gates; a private Doctor
dashboard preview is not a Contact notice journey.

### Doctor Number, agenda and notifications (#8 historical slice; integrated locally)

The [Doctor Number evidence record](docs/integrations/doctor-number.md) documents
the historical verification boundary and current live gates. In
**Configuración → Conexiones → Número del doctor**, the authenticated Doctor
generates a six-digit code and sends it from their own phone to the Practice
Number. A code lasts 600 seconds, has five eligible guesses, and issuance is
limited to once per 60 seconds and five per rolling hour. Redemption requires
current signed sender evidence and the numeric phone-form envelope `me`; typed
numbers, media captions and historical aliases alone are insufficient. Equivalent
`52`/`521` formatting is recognized without rewriting the preserved source phone.

The verified Doctor Number supports read-only agenda answers and operational
notifications, not mutations. The earlier #8 statement that no agenda or
notification implementation existed is superseded. Saved binding is ownership
evidence, not proof of message delivery. Incoming/prepared evidence can retain a
submitted challenge code under its separate inbox retention policy; do not claim
that every stored copy is encrypted or log it. Real sender/`me` evidence, pairing,
privacy/consent and provider launch gates remain open.

### Google Calendar and Meet (#9 historical slice; live acceptance still open)

See [Google connection, lifecycle and operations](docs/integrations/google-calendar.md).
The Doctor connects through the direct OAuth/Calendar REST integration, explicitly
selects a Booking Calendar and zero or more Blocking Calendars, and can run the
durable capability/cleanup check. Calendar availability blocks conflicts;
Appointment events and Meet work reconcile through durable current-state checks.
Only a successful provider conference with a validated join URL can supply a
Meet link. A revoked connection, ambiguous result or failed cleanup stays visible;
cleanup retry cannot create current capability. Ownership metadata is not proof
of account identity.

The historical browser-boundary incident is deliberately preserved: an earlier
run escaped page-only interception and reached Google's real `invalid_client`
error page with synthetic authorization parameters; the exact live request count
is unknown. No real account credentials, consent, token exchange or Calendar event
was observed. The incident was disclosed, and later independently reviewed
whole-browser OS-confinement evidence does not erase it. See the
[dated incident and acceptance record](docs/integrations/google-calendar.md).
Request tracing omits caller URLs/queries/headers and provider credential bodies;
`RUST_LOG` cannot widen the fixed filter. Native Doctor pages retain same-origin
referrer behavior; only the outer OAuth callback uses no-referrer. Operator/proxy
logs need their own query-suppression policy.

**Historical/live boundary:** #4–#9's staged records, tests, incidents and
unresolved provider questions remain linked evidence, not proof of current
production readiness. Do not launch old browser scripts that bypass the recorded
OS-network confinement; do not infer live WAHA or Google behavior from synthetic
fixtures.

## Tests

Fast unit checks (no test database required):

```sh
cargo test --locked
```

This excludes ignored database tests and is **not full application acceptance**.
Full local acceptance includes Doctor, Practice, WAHA/Contact conversation,
Appointment, payment/Receipt, Calendar/Meet, reminder, recovery and retention
tests against dedicated real Postgres. Do not treat an ignored test as a pass.

### Disposable Postgres fixture

`compose.test.yml` runs only Postgres 17. It uses tmpfs, no persistent/shared
volumes, no fixed container name, and a loopback-only port (default `15439`). Data
is disposable and is lost when the container stops. Do not use or reset the
persistent `example` database in the legacy `docker-compose.yml` for tests.
That legacy Compose file is not the isolated test fixture or a safe preview recipe.

In one shell, generate a temporary password without printing it, and start a
separate Compose project. Keep shell tracing disabled so secrets are not logged:

```sh
set +x
export POSTGRES_TEST_PASSWORD="$(openssl rand -hex 32)"
export TEST_POSTGRES_PORT=15439
export TEST_DATABASE_URL="postgres://medical_test:${POSTGRES_TEST_PASSWORD}@127.0.0.1:${TEST_POSTGRES_PORT}/medical_assistant_test"
docker compose --env-file /dev/null -p medical-assistant-tests -f compose.test.yml up -d --wait
cargo test --locked -- --include-ignored
```

Use an unused `TEST_POSTGRES_PORT` and a unique project name for concurrent runs;
replace `medical-assistant-tests` consistently in both setup and cleanup. Do not
regenerate the password while reusing an already running fixture. `--env-file
/dev/null` prevents Compose from implicitly reading the application `.env`.

The acceptance tests require **explicit `TEST_DATABASE_URL`** and never fall back
to `DATABASE_URL` or `.env`. The actual database name must be
`medical_assistant_test` (or another name ending in `_test`). Use only a disposable
database even if another database happens to have that suffix. Test URLs do not
accept query parameters or fragments. Tests create a unique schema per test to isolate their data and initialize their own fixtures;
no application migration command is needed for this test setup.

The public router composes `Config` and `Db` without requiring a server listener.
A test-only persistence fixture makes an HTTP response depend on real database
data through the same router middleware. It is not a production diagnostic route
or a product table, and it does not change the static health endpoint. These
checks need no live WhatsApp, Google, or other provider credentials. Google tests also use
actual synthetic OAuth/Calendar HTTP and real worker/DB crash boundaries; see the integration
record for conservative single-dispatch and cleanup evidence. Provider-dependent route and
streamed-body limits, captured secret-safe logs, callback timeout envelopes, exact scopes,
credential rotation and original-target cleanup are exercised without production fixtures.

Doctor acceptance tests enroll isolated fixtures through the real enrollment
service, then submit real passwords and generated TOTP codes through the full
Axum router. They cover missing/wrong/expired factors, concurrent replay, consumed
enrollment codes, singleton setup races, encrypted secrets, hashed session tokens,
30-day absolute expiry, restart-persistent fixed-window limits before bcrypt,
CSRF/origin failures without mutation, logout, challenge expiry/issuance bounds,
return navigation, protected deep links, and escaping in the actual SSR views.
Storage-failure tests force a session-insert failure after TOTP/challenge changes
and verify full rollback and successful retry; lookup failures deny access without
revoking the existing session or exposing database errors. The response-header
regression preserves native browser form `Origin` behavior.
Practice acceptance tests use those same real enrollment/login helpers and exercise
all four steps, approved defaults and null decisions, edits, repeated Service UUIDs,
exact-money overflow, day/range validation, explicit IANA/DST examples, CLABE leading
zeros, stored/reflected escaping, CSRF/origin rejection without effects, per-step
atomicity, database failures, and the distinct body limits. Persistence coverage
closes the original database pool, creates a new connection pool and router, reruns
migrations, and checks the same Practice, Services and authenticated session. This
is a real database reconnection test, **not a process-restart or browser test**.
The existing legacy-migration rejection/data-preservation checks remain in place.
WAHA tests add a synthetic loopback HTTP service that checks the exact API header and
operations, all known/unknown states, rejected redirects/non-GOWS responses, incremental
body bounds, protected PNG, and zero outbound calls on failed session/CSRF checks. HMAC,
deduplication, intake rollback and preparation use real Postgres. Advisory gates and audit
triggers prove concurrent SKIP LOCKED, cancellation rollback/new-pool recovery, exactly one
committed preparation effect and persisted retry/terminal diagnostics. Gated fixtures alone
use four pooled connections. These tests do not contact the actual WAHA service or establish live provider capabilities. Google integration tests use synthetic OAuth/Calendar HTTP and real worker/database crash boundaries.

Current database-backed integration coverage includes Appointment lifecycle and concurrent Holds, rescheduling/cancellation, Receipt copying/review and late outcomes, explicit Doctor verification, Calendar/Meet failure/recovery, Reminder/attendance separation, manual refund idempotence, Contact Takeover/recovery and retention association races. Run all ignored tests for full local acceptance.

The historical #7 sender suite remains a subset of the current application tests:
it exercises real-router Followup/queue atomicity, media precedence,
aliases/conflicts, lease/CAS/cancellation, persisted pacing/caps, early/late
acknowledgements and one-POST uncertainty. A loopback PostgreSQL protocol relay
drops a real successful COMMIT response and verifies that no send follows the
ambiguous storage result. It requires the disposable Postgres fixture and does
not change application TLS or provider settings. If host resources require it,
use `--test-threads=4`, never count skipped tests as passes. Controlled clocks
avoid sleeps; authentication is not mocked. Operator artifact permissions and
command rejection have separate local tests. Real Doctor enrollment and
private-HTTPS deployment remain human steps.

After testing, remove only this fixture project, using the same shell variables:

```sh
docker compose --env-file /dev/null -p medical-assistant-tests -f compose.test.yml down
unset TEST_DATABASE_URL POSTGRES_TEST_PASSWORD TEST_POSTGRES_PORT
```

This cleanup does not target the base Compose project, persistent volumes, or
Tailscale routes. Do not run a reset/drop against an existing application database.

Additional local checks:

```sh
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
cargo build --locked --bins
```

## Changelog and evidence

**Verification recorded 2026-10-16 and 2026-10-19.** The
integrated local application now covers the Contact, Appointment, Receipt,
Payment, reminder, Doctor Number/agenda, Google Calendar/Meet, recovery, Takeover
and retention paths described above. The 10 migrations and current job kinds are
listed in this README rather than treating the older #4–#9 slice boundaries as
current product gaps.

- Final Rust verification on 2026-10-19: `cargo fmt --all -- --check`,
  `cargo clippy --locked --all-targets -- -D warnings`, and
  `cargo test --locked -- --include-ignored --test-threads=4` passed. The
  full run reported 244 library plus 1 CLI test (245 total), zero failures
  or ignored tests, including the actual 120-second synthetic send deadline.
- Native rehearsals used synthetic enrollment, password+TOTP and private HTTPS
  in OS-network-confined Chromium. The 2026-10-19 run exercised the actual
  release image, all four Practice forms, signed in-person booking, escaped
  names, native unpaid cancellation/Hold release and the past list. The earlier
  2026-10-16 run also exercised Contact rescheduling and conversation history.
- For explicitly seeded historical video fixtures, signed intake copied a
  68-byte PNG, recorded timely/not-late evidence and protected the under-review
  Hold indefinitely. Receipt review passed desktop and 360px mobile inspection
  without horizontal overflow. A native browser download completed with all
  68 bytes. Doctor verification produced `confirmed` / `verified` with no
  invented Meet; paid cancellation retained verified payment. Native external
  refund resolution cleared attention and added one history entry without
  changing the cancelled status, verified payment or scheduling revision.
  Logout denied both review and file access; the public notice remained
  provisional with no Doctor controls. Final-image smoke also exercised the
  corrected pending-payment labels independently of Receipt review state.
- Host-side private HTTPS returned HTTP 200 with certificate verification
  enabled and successful (`TLS_VERIFY=0`, no insecure flag). ThinkPad TLS
  succeeded on 2026-10-16; its 2026-10-19 SSH probe timed out, so current
  client-side reachability is unverified. Runtime audit recorded zero provider
  POSTs; the default-OFF outbox made zero irreversible dispatch commitments.
- The isolated operations rehearsal exercised the actual Compose Postgres init,
  all 10 migrations, separate app/WAHA roles, an age-encrypted full-cluster dump,
  strict batch SFTP transfer and checksum, and an isolated tmpfs restore with no
  app, WAHA, worker or consumer. A synthetic 67-byte Receipt was byte-preserved;
  checksum and age-authentication corruption were rejected. The systemd units
  parsed successfully but no unit or timer was installed, enabled or started.
  This is not live WAHA session/media storage or restore acceptance.
- The final application image built locked release `server` and `db` binaries
  using Rust 1.88 and pinned `cargo-chef` 0.1.71, correcting the observed
  Rust 1.87 builder failure. Its compilation-only build-context allowlist
  excludes operator environment files and local evidence. Both the server and
  packaged `/app/db migrate` ran against the owned disposable Postgres fixture
  with no external network route. The ignored container `RUST_LOG` setting was
  removed; application logging retains its fixed credential-safe filter.
- Verification cleanup removed the owned application/shim/browser/bridges,
  disposable Postgres project/network, private scratch credentials, image tag
  and temporary HTTPS routes. Unrelated Serve routes and the existing Funnel
  configuration were preserved; no timer or production service was activated.
- No production deployment or production Doctor enrollment, WAHA pairing/storage
  cutover, Google consent/provider acceptance, outbound messaging activation,
  legal approval or product release has been performed. The privacy notice remains
  provisional.
- **Close-out verification 2026-10-20 (new entry; historical entries above are
  preserved):** re-verification passed 254 tests (253 library + 1 CLI), zero
  failed or ignored, including the 120-second synthetic send, plus formatting
  and strict Clippy. This adds the WAHA pairing-code flow
  (`POST /settings/whatsapp/request-code`, synthetic provider only; live
  pairing remains gated, including proof of code-safe provider logging) and
  offline Doctor/Google maintenance key rotation with the stable encrypted
  fingerprint subkey, proven by real-router tests, an actual-binary
  migrate/rotate/login/issuance smoke against a disposable database, and
  plain-pg_dump snapshot restore rehearsals per key epoch (matching epoch
  logs in; wrong epoch fails closed). Those snapshots are not the
  age-encrypted backup tooling. Real enrollment, provider acceptance, legal
  approval, deployment, messaging activation and any real rotation remain
  operator decisions. The commercial brief is dispositioned row by row in
  [docs/brief-traceability.md](docs/brief-traceability.md).
