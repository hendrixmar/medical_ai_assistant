# WAHA / GOWS: application contract and live acceptance gates

Evidence date: 2026-10-14. Target **WAHA 2026.9.2**, source commit
[`90ba445a0e765701c8910f0fac08176250c3e0fd`](https://github.com/devlikeapro/waha/tree/90ba445a0e765701c8910f0fac08176250c3e0fd).
One deployment, one Practice Number, one existing session named **`default`**, engine
**GOWS** (ADR-0001). This is an application slice, **not completed issue #6 live acceptance**.
No provider credentials were read, no live provider calls were made, and no pairing,
webhook registration, storage change, container restart, or deployment was performed.

**Subsequent #7 application integration:** see [conversation/outbox operations](conversation-outbox.md)
and [pinned messaging source](waha-messaging.md). The #6 evidence below remains historical
and its live gates remain open. #7 adds optional ID/seen/typing/text calls and signed
`message.ack` intake; none is authorized merely by configuring or pairing #6.
The subsequent [Doctor Number record](doctor-number.md) uses the existing protected status
GET plus current signed sender/optional numeric envelope `me` for ownership proof. It adds
no provider operation, pairing or automatic sending opt-in; live sender/`me` evidence remains
unverified.

## Current application and media/storage evidence — approved issues #10–27

The #6 intake/preparation seam is now part of the full application: signed incoming messages feed Contact conversations, deterministic booking/rescheduling/cancellation, Appointment and Receipt handling, and a shared paced outbox. Receipt media is copied promptly from WAHA only from an exact same-origin URL; it is then served through the authenticated Doctor route. Conversation/Appointment recovery and retention workers are implemented, while uncertain committed text is never resent. Outbound messaging and automatic Takeover remain OFF by default.

Compose sets WAHA's returned-media origin to **`WAHA_BASE_URL=http://127.0.0.1:3000`**, matching the application's `WAHA_BASE_URL` and its `network_mode: service:waha` loopback access. The pinned [WAHA configuration docs][waha-config] state that this value constructs `media.url`; the pinned [`2026.9.2` environment example][waha-env] defaults to `http://localhost:3000`. The application accepts fetched media only when URL origin exactly equals its configured base origin, so `localhost` does not match `127.0.0.1`; there is no hostname fallback. This fixes the returned-media URL mismatch. It is a configuration/source fact, not observed live WAHA media delivery.

WAHA Core 2026.9.2's pinned source and official storage docs support both GOWS PostgreSQL session storage (`WHATSAPP_SESSIONS_POSTGRESQL_URL`) and PostgreSQL media storage (`WAHA_MEDIA_STORAGE=POSTGRESQL`, `WAHA_MEDIA_POSTGRESQL_URL`). They are separate from the application's Postgres database/role. The official [storage docs][storage-docs], [Core session manager][manager], [GOWS auth factory][store] and [PostgreSQL media factory][media-store] document the capabilities; [licensing policy][plus] places storage in Core. The `devlikeapro/waha:2026.9.2` image was not present locally and was not pulled or started; there was no image inspection or real WAHA session/media database.

An isolated operations rehearsal did exercise the actual application-Postgres encrypted backup/restore path: the real CLI applied all 10 migrations, then age-encrypted the synthetic database dump and transferred it by strict SFTP to an owned temporary ThinkPad directory. The isolated restore ran with no app/provider/worker/ports and preserved a synthetic **67-byte PNG Receipt** exactly; checksum and ciphertext/authentication corruption were rejected before restore. A separate synthetic AES-256-GCM Google credential-shaped row passed independent decrypt/authentication after restore. This is not a WAHA-created session/media database backup, WAHA authentication-state recovery, returned-media fetch, or same-account session restoration test.

Shared full local verification on 2026-10-16 passed 245 tests (244 library + 1 CLI), zero failed or ignored, using synthetic local providers and real Postgres. Native private-HTTPS/OS-confined Chromium used synthetic Doctor enrollment and password+TOTP login with a loopback provider, exercised in-person booking/rescheduling, copied a signed 68-byte PNG Receipt and clicked its protected download link. No live WAHA request, pairing, session/media restoration, provider restart, or image-container restore was performed or is implied by those results.

The #6 application details below remain a historical capability/evidence record. Their stage-specific statements that prepared messages were not yet handled, Receipt URLs were never fetched, and Appointment/Receipt journeys were out of scope refer to that earlier slice; later integration adds those consumers. The real WAHA pairing/session/media/restart and delivery gates below remain open.

## Evidence levels

| Capability | Primary-source evidence | Synthetic/application evidence | Live Practice evidence |
| --- | --- | --- | --- |
| Session state/start | [Session controller][sessions], [GOWS][gows] | Protected real-router tests call a local synthetic HTTP listener; validate API header, fresh states, start acknowledgement, errors and bounds | Not collected; supplied context says default stopped/unpaired, not newly verified |
| QR pairing | [Auth controller][auth], [GOWS][gows] | GET image-JSON decoded to bounded protected PNG; one-pixel synthetic PNG only | Human-controlled Practice phone/pairing authorization still needed |
| Pairing code | [Auth controller][auth], [GOWS][gows] | Implemented against the pinned contract (see below); synthetic real-router tests only | Availability on actual Practice account unverified |
| Passkeys | [Auth controller][auth], [Auth DTO][auth-dto] | Honest required/confirmation-required states only, no assertion generation | Conditional human authenticator flow unverified |
| Postgres session storage | [Core manager][manager], [GOWS Core factory][store], [storage docs][storage-docs] | Application inbox durability tested in real Postgres; this does **not** test WAHA authentication storage | Existing deployment reportedly filesystem-backed; required cutover/restart proof blocked |
| Authenticated webhook intake | [Webhook sender][sender], [GOWS][gows] | Raw-body HMAC vectors, full-router intake, real PG transaction/duplicate/failure tests | Callback provisioning and real delivery unverified |
| Incoming calls | [GOWS][gows] emits `call.received`, `call.accepted`, `call.rejected` | Signed call events explicitly ignored with no job/reply | Delivery on this Practice Number unverified; consensual real call evidence required |

Rolling website documentation is secondary to the pinned source. In particular,
the docs pairing feature table says POST QR incorrectly: the controller is **GET**.
Synthetic contracts establish application behavior, not successful real pairing,
provider restart persistence, or freedom from unofficial-client bans/re-pairing.

## Application configuration and requests

All three settings must be absent, or all valid:

- `WAHA_BASE_URL`: canonical root origin, no trailing slash, path, userinfo, query or
  fragment. HTTPS except explicit `http://127.0.0.1`, `http://[::1]`, or
  `http://localhost` (optional port) for host-internal/synthetic use.
- `WAHA_API_KEY`: actual plain API request credential, **not its stored SHA-512
  hash**; nonempty and header-valid. Never send it to the browser.
- `WAHA_WEBHOOK_HMAC_KEY`: operator-provisioned random secret of at least 32
  characters. Literal UTF-8 bytes, **not base64-decoded**. No default.

Absence leaves login/settings/health usable and WhatsApp unconfigured. Partial,
empty or malformed supplied settings fail startup with a value-free diagnostic.
The shared client is built once per application composition, verifies TLS via
reqwest rustls, ignores proxy environment variables, refuses all redirects, uses
2-second connect/5-second whole-request timeouts, and reads responses incrementally
with limits: status/start 256 KiB, QR JSON 1.5 MiB, decoded PNG 1 MiB. No raw provider
errors, JSON, API keys or QR bytes are logged or rendered into HTML.

The #6 connection UI performs only these operations (the separate opt-in #7 sender is
specified in the [application record](conversation-outbox.md)):

1. `GET /api/sessions/default` for fresh reported state and optional sanitized phone.
   Recognized non-GOWS engines fail closed; stopped sessions may report `engine: {}`.
   Unknown state is not working. Raw push names and unknown state strings are not shown.
2. `POST /api/sessions/default/start` after Doctor session + exact Origin + CSRF,
   and only after a fresh STOPPED response. It starts the existing session, never
   creates/reconfigures one. A successful acknowledgement redirects to fresh status;
   it never establishes WORKING. There is no automatic start on GET or startup.
3. `GET /api/default/auth/qr?format=image`, `Accept: application/json`, after fresh
   SCAN_QR_CODE. `{mimetype:"image/png",data:"<base64>"}` must decode within bounds
   and have a PNG signature. The browser gets only `/settings/whatsapp/qr` through
   the Doctor boundary with `no-store`, never provider URL/key or a data URI.
4. `POST /api/default/auth/request-code` via the protected
   `POST /settings/whatsapp/request-code` (Doctor session + exact Origin + CSRF),
   only after fresh SCAN_QR_CODE. See the pinned pairing-code contract below.

Pairing code is implemented against the pinned contract, re-read 2026-10-20:
`POST /api/default/auth/request-code` with JSON `{"phoneNumber":"<international digits>"}`
returns `{"code":"<string>"}` ([auth controller][auth], [auth DTO][auth-dto],
[GOWS `requestCode`][gows]). GOWS requires `SCAN_QR_CODE`; it briefly waits when
STARTING and otherwise rejects with provider 422. The application omits `method`
and locale fields (Web pairing; GOWS forwards only session and phone), requests a
fresh status first, and only calls request-code in `SCAN_QR_CODE`. The code is
trimmed and must be 4–32 ASCII alphanumeric or `-` characters; it is never
persisted, logged by the application, put in a URL, or treated as WORKING —
WAHA owns expiry. Exact code expiry and phone-side QR invalidation are **not**
established by the pinned application source. Actual Practice acceptance is
unverified.

Passkeys remain **not implemented** and stay honest out-of-page states. Source
supports conditional passkey challenge/assertion/confirmation endpoints, but the
challenge RP is `web.whatsapp.com`; signing cannot be silently performed at the
Practice dashboard origin. WAHA documents human/manual or extension assistance.
Pairing can still end in a passkey-required state. Neither receiving a QR/code nor
acknowledging an assertion is proof of WORKING. FAILED requires operator
attention, not automatic recovery.

**Provider-log prerequisite (blocking live pairing-code use):** the pinned GOWS
`requestCode` calls `logger.info` with the pairing code. The checked-in
`deployment/compose.yml` therefore sets `WAHA_LOG_LEVEL: warn` for the `waha`
service; pinned [`src/utils/logging.ts`](https://github.com/devlikeapro/waha/blob/90ba445a0e765701c8910f0fac08176250c3e0fd/src/utils/logging.ts)
(re-read 2026-10-20) honors exactly that env name, and treats the mere presence
of `DEBUG` (any value, including `"false"`) as enabling debug. `DEBUG` must be
absent. Before any real code request, the operator must prove the actual
image/session/debug overrides and log collectors cannot record pairing codes or
phone input; application/synthetic logs do not prove upstream log safety. Until
that proof, live pairing-code acceptance is blocked. Warn/error logging remains
available; raising verbosity later requires repeating this secret-safety check.

The authenticated page is `/settings/whatsapp`. All four dashboard routes use the
existing Doctor authorization boundary; unauthorized requests cause **zero** provider
calls. CSP allows same-origin images, no scripts. Keep `Referrer-Policy: same-origin`
for native form Origin behavior. Login return destinations are exact-allowlisted.
Even WORKING is not evidence that automatic replies or delivery are enabled. The #7
UI replaces the former unimplemented-replies copy with that delivery caveat.

## Webhook registration belongs to the operator

The application never performs session PUT/create/stop/logout/delete or changes
provider configuration. Before live acceptance an authorized operator must provision
`config.webhooks[]` with:

- URL = canonical `PUBLIC_ORIGIN` + `/api/v1/webhooks/waha`, reachable from WAHA with
  valid HTTPS and applicable private-network routing/DNS;
- `events` including `message`; #7 acknowledgement correlation additionally requires
  the explicitly authorized `message.ack` subscription;
- `hmac.key` equal to the application HMAC secret.

Pinned [session DTO][session-dto] / [controller][sessions] support configuration via
`PUT /api/sessions/default`. Inspect and **preserve all existing configuration and
webhooks** when preparing an authorized update; no replacement command is supplied
here. Provision credentials privately, never in chat, source, request logs, screenshots,
or shell arguments/history. A connection-status response alone proves none of callback
registration, delivery, storage selection or bot readiness. No implicit repair is performed.

## Durable intake

Public `POST /api/v1/webhooks/waha` is outside cookie/CSRF middleware: its authentication
is `X-Webhook-Hmac`, exactly one **128-character lowercase hex HMAC-SHA512** over the
exact received bytes (2 MiB maximum), constant-time library verification before JSON
parsing or database effects. If supplied, the single algorithm header must be `sha512`.
The pinned sender signs `JSON.stringify` UTF-8 bytes, so whitespace changes require
re-signing. Header request ID / envelope event ID are not message identity.

Only `event=message`, `session=default`, `engine=GOWS`, `payload.fromMe=false`, direct
numeric `@c.us` or `@lid` identities produce work. Signed outgoing, group, broadcast,
newsletter, other-session/engine and non-message/call/status events return
`{"outcome":"ignored"}` without effects. Ignored events still require valid HMAC.
Required accepted-message fields are validated, including bounded payload ID,
identity, integer server timestamp and known optional content field types.
Unfamiliar content inside a valid incoming message is preserved and prepared as
`other`, not dropped.

- `401`: absent/duplicate/malformed/incorrect signature or algorithm.
- `400`: signed malformed envelope/accepted message.
- `413`: raw body too large.
- `503`: unconfigured intake or unavailable persistence; never success on failed commit.
- `200`: committed `accepted`, durable `duplicate`, or authenticated `ignored` outcome.

`inbox_messages.message_id` is **payload.id**, case preserved, at most 512 bytes.
`original_chat_identity` remains unchanged. `resolved_phone_identity` is canonical
`@c.us` from that phone identity or optional `_data.Info.SenderAlt` (normalizing
`@s.whatsapp.net`, [official identity helper][identity]); LID digits are never a
phone inference. `server_timestamp` is payload timestamp **seconds**, distinct from
application Clock arrival (`arrived_at` timestamptz). #8 refines new arrival writes to one
`Clock.now_millis()` sample, retaining floor-second preparation eligibility and immutable
arrival on duplicates; no old rows or timestamp columns are rewritten. Envelope milliseconds remain
in `raw_envelope` bytea, along with all authenticated bytes.

A transaction inserts the inbox row using `ON CONFLICT DO NOTHING RETURNING`, then
only the winner inserts its unique `prepare_inbox` job. Both commit **before 200**.
Duplicates never overwrite original bytes, identities, times, preparation, or jobs.
The #6 migration contained only the inbox/preparation tables. The later #7 migration added Contacts, Followups, outbox and acknowledgement evidence. At the #6 checkpoint, Receipt downloading and Appointment/Receipt journeys were outside that slice; later issues #10–27 add those consumers. This historical migration boundary does not describe current application scope.
Protect this sensitive database and its backups; raw messages are intentionally
retained for recovery, not printed into logs or exposed via a debugging API.

## Historical #6 preparation worker and failure semantics

Public production seam: `whatsapp::jobs::run_once(&Db, &dyn Clock)` returns Idle,
Prepared(id), RetryScheduled(id), or Failed(id); callers must not log sensitive IDs.
At the #7 checkpoint, the #6 preparation seam was polled by the process-owned 250ms database loop, with conversation/reconciliation on that database loop and a separate outgoing HTTP loop. Router constructors spawn no workers. Shutdown aborts/awaits tasks, rolling back interrupted transactions; Postgres also rolls back interrupted process connections. DB statement timeout is 3 seconds and lock wait 2 seconds per transaction; pool acquisition uses configured `DATABASE_TIMEOUT`.

The #6 job claimed one due `prepare_inbox` row with `FOR UPDATE SKIP LOCKED`, parsed the retained envelope, and transactionally stored canonical text/media/other input as `ready_for_conversation`. At that checkpoint it was not yet handled/replied; #7 added the unique `process_conversation` job in the same transaction. The later full application consumes that input and continues into Appointment and Receipt journeys.

At the #6 preparation stage, media metadata projected only documented `url`, `mimetype`, `filename` from pinned [WAMedia][media] (bounds 8192/256/1024 bytes). Unknown/error/S3 fields stayed in raw storage, not the projection. That preparation step did not fetch media URLs. Later Receipt ingress fetches only a bounded, exact same-origin WAHA URL for immediate application-owned Receipt copying; URLs are never followed across origins or redirects. `hasMedia` takes precedence; otherwise a nonempty body is text, absent/null/empty body is other. Bodies are preserved without trimming, bounded to 1 MiB; malformed known fields fail validation.

On a work failure, roll back the savepoint (including canonical writes), keep the
outer job lock, then commit attempts, safe error code and next due time. Database work
errors retry with exponential 5/10/20/40 seconds between up to **five total attempts**
(formula capped at 300 seconds); the fifth failure is retained terminal `failed`.
Invalid stored data is terminal immediately with `invalid_stored_message`; database
failures use `preparation_database_error`. No raw DB errors are persisted. A failure
preventing diagnostic commit rolls back the outer transaction: pending work remains
recoverable, with sanitized telemetry, not a falsely claimed persisted failure.
The #6 preparation job itself had no leases, stale claims, generic payload registry, terminal-failure retry or operator retry endpoint. This stage-specific limit does not describe the later domain-specific Appointment, Receipt and recovery workers.

Tests use full Axum HTTP + real isolated Postgres and an actual loopback synthetic
WAHA HTTP listener. PostgreSQL triggers/advisory gates/audit rows demonstrate concurrent
SKIP LOCKED, exactly one committed preparation effect, cancellation after canonical
write, rollback, new-pool recovery, final-success failure, persisted backoff and terminal
retention. No production pause hooks, mock database, noop processor or SQL-string-only
assertions. A reconnect/cancelled task test is **not** a real WAHA/container/process
restart test. No #6 private-HTTPS browser or deployed-provider test is claimed by these
automated application tests. The supplied UI worker separately reports isolated browser
render cases with a one-pixel synthetic PNG, not real pairing/authentication evidence.

Historical issue #6 application verification passed all 79 tests, formatting,
strict Clippy and binary builds. Review found no must-fix code defects; follow-up
coverage exercises start/QR failures after successful status, streamed QR bounds,
a stalled provider deadline, signed field rejection without effects and exact-limit
acceptance through canonical preparation. A real Chromium/private-HTTPS run with synthetic
credentials and a loopback provider verified native login/start, protected QR image
loading under CSP, fresh reported states, CSRF rejection before provider mutation,
sanitized provider failures and signed intake without Doctor cookies. A PostgreSQL
advisory gate held preparation uncommitted after the webhook acknowledgement; the
application process was forcibly killed, then restarted against the same disposable
database. The worker recovered the message and committed exactly one audited
preparation. Concurrent changed-envelope duplicates preserved original bytes,
identity and both timestamps. Doctor sessions and Practice configuration also
survived this real application-process restart. This is **not** a WAHA/provider
restart, real QR pairing, or live callback test.

The tailnet HTTPS endpoint was checked with valid TLS from this host. A separate
ThinkPad check could not be completed because its SSH connection timed out, so
client-side reachability is unverified for this run. Temporary application/provider
listeners, the test HTTPS route, synthetic browser state and disposable database
were removed; pre-existing Tailscale routes were preserved.

## Postgres storage and operator-blocked live gates

[Official storage docs][storage-docs] plus pinned Core source specify
`WHATSAPP_DEFAULT_ENGINE=GOWS` and **`WHATSAPP_SESSIONS_POSTGRESQL_URL`**. This is WAHA
session storage, not media storage or the application inbox database. Default namespace
`gows` creates databases **`waha_gows`** and **`waha_gows_default`**; the URI's initial
database bootstraps creation. Provision an isolated role/databases with the required
database create/drop permissions, approved endpoint and TLS policy. Do not give these
powers over application data. Mongo settings take precedence if simultaneously present;
preserve and review existing namespace/storage settings rather than assuming defaults.

**Not a Plus blocker:** [official licensing policy][plus] makes all storages available
in Core since 2026.6.1, corroborated by this version's Core factory. The real mismatch
is supplied existing **filesystem** persistence, not required Postgres. No established
automatic filesystem-to-Postgres session migration was found. Simply changing a URI
is neither a safe migration nor persistence proof.

Before full issue #6 acceptance the operator still needs to authorize/provide:

1. Postgres endpoint, isolated role/database permissions and TLS policy; backup,
   cutover/recovery plan and restart window for existing filesystem-backed WAHA.
2. Private API/HMAC provisioning and HTTPS callback reachability; configuration
   update that preserves current session configuration/hooks.
3. Doctor-controlled Practice phone/account and explicit pairing authorization,
   including any human code/passkey steps actually required.
4. Real incoming-message delivery and durable acknowledgement evidence without
   exposing private content or pairing artifacts.
5. **Same account WORKING after container recreation on Postgres without re-pairing.**
6. Consensual actual Practice Number incoming-call delivery evidence; no call response is enabled from synthetic fixtures. Issues #21/#22 later implement Contact-scoped Takeover and bounded recovery/backfill application behavior against synthetic providers, but live sender attribution, real recovered messages and provider restart remain unverified operator gates.

Pinned [webhook sender][sender] uses process-memory Axios retries (default 15 retries,
constant two-second interval), not a durable queue. **WAHA restart can lose pending
webhook deliveries** before they ever reach this inbox. Application-side durability
cannot eliminate that upstream gap or unofficial-client ban/re-pair risks. Keep these
limitations explicit; do not close issue #6 from code, QR display, synthetic WORKING,
or application-database tests alone.

[sessions]: https://github.com/devlikeapro/waha/blob/90ba445a0e765701c8910f0fac08176250c3e0fd/src/api/sessions.controller.ts
[session-dto]: https://github.com/devlikeapro/waha/blob/90ba445a0e765701c8910f0fac08176250c3e0fd/src/structures/sessions.dto.ts
[auth]: https://github.com/devlikeapro/waha/blob/90ba445a0e765701c8910f0fac08176250c3e0fd/src/api/auth.controller.ts
[auth-dto]: https://github.com/devlikeapro/waha/blob/90ba445a0e765701c8910f0fac08176250c3e0fd/src/structures/auth.dto.ts
[gows]: https://github.com/devlikeapro/waha/blob/90ba445a0e765701c8910f0fac08176250c3e0fd/src/core/engines/gows/session.gows.core.ts
[manager]: https://github.com/devlikeapro/waha/blob/90ba445a0e765701c8910f0fac08176250c3e0fd/src/core/manager.core.ts
[store]: https://github.com/devlikeapro/waha/blob/90ba445a0e765701c8910f0fac08176250c3e0fd/src/core/engines/gows/store/GowsAuthFactoryCore.ts
[sender]: https://github.com/devlikeapro/waha/blob/90ba445a0e765701c8910f0fac08176250c3e0fd/src/modules/waha-webhook/WebhookPlugin.sender.ts
[identity]: https://github.com/devlikeapro/waha/blob/90ba445a0e765701c8910f0fac08176250c3e0fd/src/apps/chatwoot/waha/engines.ts
[media]: https://github.com/devlikeapro/waha/blob/90ba445a0e765701c8910f0fac08176250c3e0fd/src/structures/media.dto.ts
[storage-docs]: https://waha.devlike.pro/docs/how-to/storages/
[plus]: https://waha.devlike.pro/docs/how-to/waha-plus/

[waha-config]: https://waha.devlike.pro/docs/how-to/config/
[waha-env]: https://raw.githubusercontent.com/devlikeapro/waha/90ba445a0e765701c8910f0fac08176250c3e0fd/.env.example
[media-store]: https://github.com/devlikeapro/waha/blob/90ba445a0e765701c8910f0fac08176250c3e0fd/src/core/media/psql/MediaPsqlStorageFactory.ts
