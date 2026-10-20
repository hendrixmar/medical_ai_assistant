# Product

<!-- impeccable:product-schema 1 -->

## Platform

web

## Users

The primary user is one independent Doctor in Mexico, who owns one Practice and makes its decisions. Contacts communicate with the Practice through WhatsApp in Spanish. A Contact may arrange an Appointment for another person; a Patient is the attendee named on that Appointment, not a persistent clinical record.

## Product Purpose

The current local application is an end-to-end administrative system for one Practice and one Doctor: it handles Contact conversations, Appointments, Receipt evidence and Doctor decisions through a server-rendered dashboard. This implementation is not an approved product release or a clinical record, and it does not keep a persistent Patient registry.

## Operating Context

- One deployment serves one Practice and one Doctor; no shared multi-Practice accounts or roles.
- Appointments may be in person or by video. Only video Appointments require payment before confirmation.
- A Receipt is evidence of a transfer, not proof that money arrived. Only the Doctor's explicit Payment Verification establishes that funds arrived.
- The Google integration calls the Calendar REST API directly, not through MCP. The application owns booking rules and factual responses; a language model only parses Contacts' messages into constrained intents.
- The Doctor's WhatsApp number and notification links are not substitutes for dashboard authentication.

## Capabilities and Constraints

### Current local application — approved issues #10–27

The application now provides the full local administrative journey: dashboard attention and upcoming Appointments; protected Contact inbox/history and Appointment list/detail/history; deterministic WhatsApp booking, rescheduling and cancellation; and availability from Practice rules, active Holds and selected Google Blocking Calendars. A Contact may have at most three Open Appointments. Appointment identity remains stable through rescheduling; concurrent Holds and revisions fence changes, while Service identity/price/duration, timezone, bank and buffer details are saved as sent. Rescheduling retains the original payment deadline. Contacts can submit image/PDF Receipts copied for protected Doctor review. A timely Receipt protects its Hold while under review; only the Doctor's explicit verification records payment as received. Rejection and Late Receipt decisions remain explicit. Paid cancellation creates a refund Attention Item; the Doctor can record that an external/manual refund was resolved, which neither transfers money nor reverses verified payment.

The application also runs Appointment calendar synchronization, Meet creation, conflict/reconciliation and reminders; sends Doctor notifications through the verified Doctor Number and answers its agenda questions read-only; and supports Contact-scoped Takeover, WAHA recovery and retention processing. One revision- and Doctor-binding-fenced outbox handles conversation, status, reminder, Doctor notification and payment-detail messages. Takeover suppresses conversational output for that Contact; essential operational notices remain eligible. Outbound sending and automatic Takeover default OFF.

Claude is an optional, fixed-production-endpoint, closed read-only preference parser: use requires a complete `CLAUDE_API_KEY` and explicit `CLAUDE_MODEL`; there is no implicit model default. It cannot decide availability, payment or mutations. Factual replies are rendered by the application. Selected Google calendars and provider results remain the source of calendar truth: unavailable or failed Google work stays requested/pending/failed as appropriate, and no Meet URL is fabricated.

The 90-day deletion of associated message text/media waits until every associated Appointment is terminal and the retention graph is rechecked. Structured facts/history are retained at least five years; no destructive post-five-year purge is defined while legal disposition remains unresolved. Unassociated evidence stays visible for attention without an invented deletion deadline. The public privacy notice remains **PROVISIONAL**; privacy, consent, retention, deployment and provider/legal approvals remain release gates.

Final local verification on 2026-10-19 passed 245 tests (244 library and 1 CLI),
zero failed or ignored, plus formatting and strict all-target Clippy. Real-router,
Postgres and synthetic-provider tests cover concurrent booking, explicit
natural-language choice, under-review rescheduling, Calendar/Meet recovery,
reminders, attendance, manual-refund idempotence, authorization and retention races.

Actual-image native acceptance also passed synthetic password+TOTP login, all
four Practice forms, signed booking, Receipt desktop/mobile review and completed
protected download, explicit Doctor verification, paid cancellation/manual refund,
logout denial and the public provisional notice. Google-disabled verification
did not invent a Meet link. See the [canonical evidence and remaining
operator gates](README.md#changelog-and-evidence), including the historical
ThinkPad TLS result and the current client-side reachability limitation.

The historical issue #4–#9 entries below describe their original implementation checkpoints only. Their former unavailable-feature statements and counts do not describe the current application; the integration records retain the historical evidence and incidents.

### Historical issue #4 application slice

Server-rendered Spanish password-and-authenticator login and an attention-first dashboard are integrated with operator-only Doctor enrollment, encrypted TOTP storage, replay protection, persistent login limits, opaque 30-day sessions, and explicit login/write CSRF protection. Verification uses full-router tests with isolated Postgres, the actual enrollment CLI, and a private-HTTPS browser flow with synthetic credentials, including process restart and logout revocation. This is local implementation evidence, not a production release or real Doctor enrollment.

The issue #4 dashboard shell did not yet contain real Appointment or Attention Item data. Those original placeholders were superseded by the current protected dashboard and Appointment surfaces described above; no current rows are fabricated. Rendering alone is not proof of authentication: sensitive views and actions remain behind the shared protected router.

### Historical issue #5 application slice

Authenticated configuration has four editable steps: Practice details and explicit IANA timezone, stable-ID Services with integer-centavo MXN prices, seven-day working hours and scheduling rules, and bank/Receipt Deadline settings. Each step validates and saves atomically without changing unrelated decisions. Missing choices stay unconfigured; only the approved 30-minute grid, 30-day horizon, 180-minute review margin, and 10-minute transit grace are seeded. CLABE is digit-preserving text with format validation only. Saved completion is derived from actual configuration, and displayed local-time examples identify the timezone and offset.

Full-router tests use real isolated Postgres and real Doctor authentication, including invalid inputs, escaping, unauthorized/cross-site writes, storage failure, edits, and persistence after closing and reconnecting the database pool. Independent private-HTTPS browser verification also exercised the native four-step setup and persistence across a real application-process restart; this flow passed again during issue #6 verification. All configured values were synthetic: no real Practice choices were supplied or configured.

### Historical issue #6 WAHA slice — live acceptance remains gated

The protected WhatsApp page reads the existing default GOWS session's actual reported status and can request its start with the shared Doctor session/Origin/CSRF boundary. QR images are protected/no-store; showing a QR or acknowledging start never implies WORKING. The pairing-code method is now implemented against the pinned WAHA contract (`POST /settings/whatsapp/request-code`, only in SCAN_QR_CODE; see `docs/integrations/waha.md`), with actual Practice acceptance unverified and live use blocked until code-safe provider logging is proved. Passkeys remain source-documented but not implemented here (honest out-of-page states). The #7 UI now states that a connection status does not confirm automatic delivery; outbound sending has a separate explicit operator flag.

Authenticated HMAC-SHA512 webhooks durably store direct incoming messages and exactly one preparation job in a transaction before acknowledgement. Payload IDs deduplicate deliveries; original chat identity, available phone mapping, server timestamp, independent arrival time and authenticated raw bytes remain distinct. The restart-safe preparation worker produces canonical text/media/other input marked ready for conversation, not handled/replied. That #6 slice itself added no Contacts, conversations, outgoing-message outbox or Receipt downloads; #7 below adds the first three, not Receipt downloads.

Application evidence uses the real Axum router, isolated Postgres, controlled Clock, and a synthetic HTTP WAHA listener, including rejected signatures/unauthorized provider calls, duplicate/rollback failures, real concurrent SKIP LOCKED, cancellation/new-pool recovery and retained retry diagnostics. Independent verification passed all 79 tests and a real private-HTTPS browser flow: native authenticated start, protected synthetic PNG under CSP, state changes, CSRF rejection and signed intake. After forcibly killing the application while preparation was uncommitted, restart recovered the durable message with exactly one audited preparation; concurrent duplicates preserved the original input. These are application results with synthetic provider data, not real pairing or WAHA/container-restart results. The UI separately passed isolated render checks and an independent visual review. ThinkPad reachability was not verified in this run because SSH to it timed out; temporary test resources were removed.

The [pinned provider record](docs/integrations/waha.md) documents WAHA 2026.9.2 source support for QR, codes/conditional passkeys, GOWS Postgres session storage in Core (not a Plus blocker), and incoming call events. Actual Practice Number call delivery remains unverified and calls are ignored without responses. Existing provider storage is reportedly filesystem-backed; no automatic migration is established. Authorized isolated Postgres provisioning/cutover, preserved webhook configuration, human phone pairing, real delivery and same-account WORKING after container recreation without re-pairing remain operator gates. WAHA's process-memory webhook retries can be lost on restart; bans/re-pairing remain risks. Full issue #6 acceptance is not complete.

### Historical issue #7 Contact/outbox slice — historical checkpoint; launch/provider gates remain

A deterministic Spanish menu addresses Contacts as usted. One combined privacy-prefixed response accompanies administrative prompts until trusted positive evidence proves the notice prefix was sent. At that #7 checkpoint, options 1–4 were unavailable and option 5 registered or reused one open Followup Request; there was no LLM or simulated Appointment/Receipt journey in that slice. Those feature limitations were superseded by the current #10–27 application above.

The real dashboard prioritizes Followups, then retained failed/uncertain sends, reported ack errors and identity conflicts. Normal in-flight sends are not problems: uncertainty appears only after that message's dispatch window expires, while definitive failures and reported errors appear immediately. The combined list is paginated at 25 rows. Doctor resolution reuses the existing session/Origin/CSRF boundary, closes only once and atomically queues one notification; it does not claim a personal conversation or Takeover. Evidence-backed phone/LID aliases never grant Doctor access or merge conflicting Contact histories.

Outbound sending defaults OFF, independently of WAHA configuration or WORKING. Intake, enqueueing and signed acknowledgements continue while off. Explicit opt-in requires complete provider settings and a canonical HTTPS origin. Persisted size-dependent random typing of 2–6 seconds and a configurable default 20 commitments per Contact per rolling hour are application policy, not provider-approved safe thresholds. Resolution notices share that budget. There is no additional Practice quota.

Each logical outbox item permits only one irreversible HTTP text invocation. Only pre-text preparation is retried; uncertain text is never automatically resent. A successful send is server-ack evidence, not recipient delivery. Positive acknowledgements advance monotonically, with error evidence retained separately. A crash after commitment but before network transmission may sacrifice delivery; a database/network atomic fence and exactly-once delivery are not claimed.

Final independent validation passed 123 tests, including 91 real-Postgres tests and the actual 120-second synthetic send deadline, plus formatting, strict Clippy and binary builds. Full-router tests use controlled clocks, real PostgreSQL advisory/commit gates and synthetic HTTP; a protocol relay proves that losing a successful database COMMIT acknowledgement authorizes no text dispatch. Independent review found and verified the correction for premature active-send attention; additional regressions exercise crossed-identity serialization and stale-worker errors without changing dispatch policy.

Parent also verified native login, public provisional privacy, protected/idempotent Followup resolution, CSRF, mobile layout and logout over actual private HTTPS. Default-OFF made zero provider operations. After a real process kill during a held synthetic text request, restart did not resend it; distinct replies proceeded after fixture-only dispatch-window aging. Early and reordered signed acknowledgements retained read progress and separate error attention. Session and resolved state survived restart. These checks are not live WAHA delivery, legal approval or ordinary Contact reachability proof. ThinkPad TLS passed on an earlier run, but its SSH timed out on the final rerun; final client-side reachability is unverified. Temporary services, data and HTTPS route were removed, preserving existing routes. UI-worker evidence separately covers 40 isolated render/form cases and independent visual approval.

`/aviso-de-privacidad` is publicly routed but explicitly **PROVISIONAL**, administrative-only and not legally approved; legal responsible-party/contact/ARCO details and the consent journey are pending. Ordinary Contact URL reachability and legal/consent approval remain launch gates. A private tailnet preview does not establish ordinary Contact reachability. No live provider requests, pairing, deployment or real enrollment were performed for this slice. See [the application record](docs/integrations/conversation-outbox.md).

### Historical issue #8 Doctor Number slice — historical checkpoint; live gates remain

The protected native Doctor Number page issues a six-digit, 600-second ownership challenge only on a session/Origin/CSRF-checked POST after fresh WORKING Practice evidence. Five eligible wrong guesses exhaust it; issuance is limited to once per 60 seconds and five per rolling hour across restarts. Only current signed resolved sender evidence, matching numeric envelope `me`, trusted wrote-first context and fresh arrival/processing times can bind. Historical LID aliases alone cannot resolve the current sender. Strict `52`/`521` equivalence never changes the exact original destination.

The binding survives pending/failed replacement and is committed atomically with challenge consumption, one durable reply and conversation job success. Recognized Doctor messages were read-only at this checkpoint, ahead of ordinary Followup handling: no record mutation, fabricated agenda, implicit Followup resolution, or automatic sending opt-in. The later full application adds the read-only agenda and Doctor notifications under the current binding and outbox fences; they still cannot mutate records.

Final independent validation passed 146 tests, including 113 real-Postgres tests and the actual 120-second synthetic transport deadline, plus formatting, strict Clippy, builds and tracked/new-file whitespace checks. Independent source review found no must-fix defects. A supplementary regression closes its missing-key coverage gap: an active replacement cannot redeem without the key, while existing Doctor recognition and ordinary intake continue. The UI separately passed 38 isolated cases and an independent visual review with no material fixes.

Parent also verified native login/issuance/CSRF, protected code reload, and active-code/session persistence across actual process kill/restart over valid private HTTPS. Signed synthetic LID/current-phone/Practice evidence bound the exact destination; consumed replay could not rebind and 52/521 Doctor input remained read-only. Pending replacement preserved the old binding (fixture-only issuance-time aging enabled that native action); explicit synthetic opt-in used the existing paced outbox, and another restart did not resend. Logout revocation and prior #4–#7 HTTPS/crash regressions passed. ThinkPad valid-TLS health passed. Temporary services, private data and HTTPS route were removed, with existing routes preserved.

These are application results, not live WhatsApp sender/Practice/delivery or provider-restart evidence. See [the issue #8 application record](docs/integrations/doctor-number.md). No live provider request, pairing, cutover, real enrollment, commit, push or production deployment occurred; issue #8 remains open.

### Historical issue #9 Google slice — historical checkpoint; live gates remain

Protected native Google connection/reconnection requests exactly the three approved Calendar scopes. Session-bound, single-use S256 authorization and encrypted server-side credentials reuse the Doctor's authenticated write boundary. Calendar selection is explicit: owner-role management access is candidacy, including secondary/shared-manager calendars, not sole ownership or account identity. Fresh free/busy validates selected Blocking Calendars; Booking event permissions and Meet capability require separate operation evidence.

The durable capability worker permits at most one event insert per probe. Only an acknowledged matching insert, fresh own-event read and successful actual conditional DELETE demonstrate event permissions; Meet success additionally requires the provider's valid video entryPoint. Tombstones can resolve cleanup without supplying missing permission proof. Unknown creation and ambiguous 404 responses never authorize reinsertion or false success. Unresolved original targets remain visible independently of the current result, including while Google is disabled. Bounded refresh/cleanup authorization survives lock waits, commit delays and restarts; valid already-sent token rotation is retained. Reconnection preserves Practice settings and old cleanup, but requires reselection and never silently authorizes old-target cleanup with a new grant.

Final parent verification passed **201 tests**, including every one of the 146 inherited tests and the actual 120-second synthetic transport deadline. The ordinary run passed 34 tests; its 167 explicit ignores all ran in the full suite. Formatting, strict Clippy, builds and implementation whitespace checks passed. Independent source reviews cleared the authorization and four probe corrections; isolated UI checks and visual review remain separate from runtime proof.

A fresh actual-binary native journey used OS-confined Node/Chromium and a network-isolated synthetic provider, with normal TLS and native forms over private Tailscale HTTPS. It passed CLI enrollment, login/Origin/CSRF, OAuth/callback/immediate selection, real process restarts and remote-insert crash recovery, conditional cleanup, retained attention alongside later success, revocation/reconnection, explicit cleanup-only, logout, sensitive-value scans and eight desktop/mobile captures. The provider recorded 59 requests with zero assertion errors. The real 30-second lease expired naturally; only disclosed observation/cleanup/due timestamps were aged on the disposable uncertain probe. ThinkPad valid-TLS health passed. Parent repeated the resource/provenance audit: owned resources were absent and existing routes preserved; a later unrelated HTTPS route was left untouched.

**Incident qualification:** an earlier run unintentionally reached a real Google `invalid_client` error page with synthetic authorization parameters. Exact live request count is unknown; no real account credentials, consent, token exchange or Calendar event was observed. This violated the no-live-Google boundary, was disclosed, and prompted the independently reviewed OS-confinement correction. The later safe run does not erase that incident.

At the #9 checkpoint, older #4–#8 browser scripts were preserved but not rerun; their host-network launchers must not be reused unchanged. That run's historical results, inherited automated tests and overlapping native checks are separate evidence. The later full-product #27 local rehearsal ran under approved confinement and demonstrated synthetic local behavior, not live Google acceptance. Real Google client/policy/S256 interoperability, authorized real event/Meet operations, real enrollment and deployment remain operator gates. No commit or push occurred; #9 stays open. See [Google configuration and acceptance evidence](docs/integrations/google-calendar.md).

### Release, legal and provider gates (not an unimplemented-feature list)

Local implementation does not constitute a production release. Real WAHA pairing/session and returned-media restore, live sender delivery and recovery, real Google OAuth/calendar/Meet operations, Claude provider acceptance, real Doctor enrollment, and production credentials remain explicit operator gates. The privacy notice, consent model, retention schedule and applicable legal review remain provisional/unapproved; no clinical record or patient registry is implied.

## Voice and Identity

Use plain Spanish for Doctor- and Contact-facing copy. Preserve the dashboard labels “Necesita tu atención”, “Tus próximas citas”, and “Configuración”. No established visual identity or supplied imagery exists; do not invent a medical brand, testimonial, Doctor identity, or provider endorsement.

## Evidence on Hand

Current scope is governed by the approved full application in issues #10–27 and the shared implementation contract. Repository authorities remain `CONTEXT.md`, `docs/adr/0001-one-deployment-per-practice.md`, `0002-llm-parses-app-states-facts.md`, and `0003-google-calendar-rest-not-mcp.md`. Synthetic full-router/provider evidence and native local behavior are separate from live-provider acceptance and release approval.

## Product Principles

- Put decisions needing the Doctor's attention before upcoming Appointments; keep settings secondary.
- State what is available honestly, distinguishing an unbuilt feature from an empty live queue.
- Keep the Doctor in control of payment verification and record changes.
- Prefer a simple, accessible workflow over ornamental dashboard content.

## Accessibility & Inclusion

Support desktop and mobile web, including a 360px viewport. Use labeled native controls, keyboard navigation, visible focus, readable contrast, and generous touch targets. Preserve password-manager, one-time-code, and paste support. No scripts, external fonts, or decorative motion are needed for the approved shell.
