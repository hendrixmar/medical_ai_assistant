# Google Calendar connection and capability check (#9)

This integration is part of the full local application (approved issues #10–27), but it is not live-provider acceptance or a product launch. The #9 connection/capability-check record below is a historical checkpoint; its statement that Appointment, payment, notification and reconciliation journeys were not enabled described that earlier slice, not the current local application. Keep the real Google authorization, provider and production gates open.

Source and policy qualifications: [primary-source research](google-calendar-research.md),
[ADR 0003](../adr/0003-google-calendar-rest-not-mcp.md), and [domain](../../CONTEXT.md).
The approved eligibility interpretation is **owner-role management access**, including
secondary/manager-shared calendars. This is candidacy, not proof of sole calendar
ownership, account identity, or actual `events.owned` permissions.

## Current Appointment integration — approved issues #10–27

The Doctor explicitly selects a Booking Calendar and Blocking Calendars. Those selected Blocking Calendars now affect actual Appointment availability alongside Practice hours, buffers and active Holds; absent configuration, a stale list or a Google outage is not treated as free time. Appointment events are synchronized by the durable worker against the saved Booking Calendar and stable event identity. Reconciliation, cancellation and conflict work retain their original target and current-state/revision fences rather than silently moving events to a newly selected calendar.

Meet creation is part of the video Appointment flow. A Meet link is used only when Google returns the expected successful conference result and an actual validated video entry point; no URL is synthesized. An in-person request whose availability cannot be checked remains truthfully `requested`; failed event/Meet work remains pending or failed with attention and never reverses verified payment.

The shared full local suite on 2026-10-16 passed 245 tests (244 library + 1 CLI), zero failed or ignored. Synthetic full-router/real-Postgres tests cover concurrent booking, selected-calendar availability, true synthetic Google Meet and recovery, under-review rescheduling, reminder/attendance behavior, cancellation/manual-refund idempotence, auth/CSRF and retention races. Native private-HTTPS/OS-confined Chromium used a synthetic provider; the Doctor explicitly verified a synthetic Appointment while Google was disabled, and the displayed state was `confirmed` / `verified` / `noMeet`. That is evidence of truthful local behavior, not real Google OAuth, Calendar permission, event mutation or Meet acceptance. No live Google request is included in these results.

## Operator configuration — do not provision through tests

Supply through the deployment's private environment/secret mechanism:

- `GOOGLE_CLIENT_ID`: the operator's Web application client.
- `GOOGLE_CLIENT_SECRET`: that client's confidential server-side secret.
- `GOOGLE_TOKEN_KEY`: **standard base64 encoding of exactly 32 bytes**, stable across
  restarts, distinct from `DOCTOR_TOTP_KEY`. There is no generated/default production key.
- Existing `PUBLIC_ORIGIN`: exact canonical HTTPS browser origin, without trailing
  slash/path/credentials/query/fragment. The callback is exactly
  `PUBLIC_ORIGIN` + `/settings/google/callback`.

All three Google inputs absent disables Google without breaking other journeys.
Committed pending probe cleanups remain visible on settings and the dashboard when
configuration or the shared client is absent, even after a selection change clears the
current pointer. They are labeled paused, retain their original targets, and offer no
write action. Uncommitted prepared intents are not counted as cleanup attention.
An empty, partial or malformed supplied configuration, missing origin, or reuse of
Doctor TOTP key material fails startup with a static error that contains no values.
Do not uncomment empty sample entries before provisioning. Never put secrets in
command arguments, logs, screenshots, tracker evidence, or committed files.

The stable logical key identifier is **`GOOGLE_TOKEN_KEY` / Google AEAD v1**; it is not
an account identifier or an automatic rotation scheme. AES-256-GCM uses fresh 12-byte
OS-random nonces and AAD
`medical-assistant:google:v1:<purpose>:<record>:<generation>`.
The access/refresh/expiry envelope is encrypted together (`credentials`, singleton
record `1`); each PKCE verifier is independently encrypted (`pkce`, attempt UUID).
Refresh replacements are encrypted and atomically persisted under the same generation.
A reconnect always needs a fresh access **and** refresh token pair; it never borrows
an older account's refresh token. Wrong/lost keys make credentials unavailable while
preserving ciphertext, metadata and original cleanup targets. Back up the stable key
separately from the database.

Offline maintenance rotation is implemented (operator-only; see README
"Offline maintenance key rotation"): `db rotate-google-key` re-encrypts stored
credentials under the new key in one transaction and supersedes pending OAuth
verifiers, leaving generation/selections/targets/Appointments untouched.
`db rotate-google-key --reconnect-only` needs no old key: it discards local
tokens, marks the connection `reauthorize`, makes **no provider call and no
revocation claim**, and the Doctor reconnects through the ordinary
authenticated flow with explicit calendar reselection. Neither operation is
compromise remediation or provider grant revocation.

Production destinations are compiled fixed HTTPS origins:
`accounts.google.com/o/oauth2/v2/auth`, `oauth2.googleapis.com/token`, and
`www.googleapis.com/calendar/v3`. TLS verification stays enabled. There are no proxies,
redirect following, implicit HTTP retries, provider-fixture environment variables or
production debug routes. Test-only endpoint injection exists solely in `cfg(test)`.
Bearer credentials use headers, never query parameters. Calendar IDs are encoded as
path segments; no userinfo, tokeninfo, OIDC or Google account API is called.

## Doctor journey and trust boundaries

Use native protected `/settings/google` from Configuración → Conexiones.

| Route | Effect |
| --- | --- |
| `GET /settings/google` | Fresh complete CalendarList, local outcomes and original cleanup records; no remote event mutation. Refresh is allowed. Cleanup pagination uses `cleanup_page`, 25 records per page. |
| `POST /settings/google/connect` | Persist latest session-bound OAuth attempt, then redirect to Google. Exactly five starts per rolling hour across sessions/restart. |
| `GET /settings/google/callback` | Strict bounded raw query, live initiating session, hashed state, single-use consumption, code exchange and fenced grant installation. |
| `POST /settings/google/calendars` | Fresh list and explicit zero–100 Blocking Calendars, all free/busy validations, atomic selection-version CAS. |
| `POST /settings/google/probe` | Persist one probe intent/immutable target. No event HTTP in this handler. Form includes expected current probe ID; accepted forms cannot be replayed to create another event. |
| `POST /settings/google/probes/{id}/cleanup` | Explicit bounded cleanup-only retry using the current grant against the original target; never creates an event or enables capability. At least 60 seconds between accepted retry requests. |

All writes reuse actual Doctor session, exact Origin and form CSRF checks. Callbacks
use the initiating **live session hash**, not an independent browser binding. State is
32 random bytes, valid for ten minutes; S256 uses an independent high-entropy verifier.
Latest authorization sequence wins. Session → connection → attempt/probe lock order and
commit-time generation/sequence/config/session checks prevent logout, reconnect or a
late callback from installing stale credentials. No SQL transaction spans provider HTTP.

Authorization requests exactly these three full URI scopes, offline access and
`consent select_account`, without incremental grants, login hints or identity scopes:

```text
https://www.googleapis.com/auth/calendar.events.owned
https://www.googleapis.com/auth/calendar.freebusy
https://www.googleapis.com/auth/calendar.calendarlist.readonly
```

New grants require exactly that returned scope set, Bearer type, bounded nonempty
credentials, valid provider expiry and a fresh refresh token. Missing/partial/extra
scopes fail closed. Failed/denied code exchange does not revoke or overwrite an unrelated
old connection. No project-wide revocation is performed. Successful reauthorization
advances generation, clears selections/current proof and requires explicit reselection;
unrelated Practice settings and original probe records remain intact.

The outer callback envelope is outside auth, route rejection, security headers and the
15-second global timeout: fixed empty `303 /settings/google`, `no-store`, `no-referrer`,
no resources. Trailing-slash callbacks receive the same envelope. Anonymous clean
settings navigation goes only to `/login?return_to=/settings/google`; callback queries
never enter login navigation. Other Doctor pages **retain `Referrer-Policy: same-origin`**
for native Origin/CSRF behavior. Persisted sanitized outcomes, not success query flags,
drive notices.

Only the Google settings GET document permits `https://accounts.google.com` alongside
`'self'` in CSP `form-action`, so Chromium can follow the native Connect POST redirect.
Other documents keep `form-action 'self'`; Origin/CSRF checks and all other CSP directives
are unchanged. Allowing the destination only on the redirect response is insufficient
because the browser applies the initiating document's policy.

Application request spans contain only allowlisted route/method labels and an
internally generated correlation ID. Caller request IDs, URI/query, headers, cookies,
provider bodies, calendar IDs, credentials and Meet links are not traced. Production
logging uses a fixed `off,server=info` filter; **`RUST_LOG` cannot widen it**, including
explicit dependency directives. Proxy/operator access logs must independently suppress
callback queries and credential traffic; application logging cannot sanitize another
process's logs.

## Persistence and worker ownership

Additive migration `m202610140004_google_calendar` creates exactly:

1. `google_connection`: singleton generation/token/selection versions, encrypted
   credential envelope, refresh claim/dispatch fence, current pointer, capability proof.
2. `google_auth_attempts`: immutable issuance/session/config evidence, hashed state,
   encrypted verifier and sanitized outcome; logout cannot cascade away rate history.
3. `google_probes`: immutable original calendar/event/markers/time, create commitment,
   distinct event permission/conference/cleanup evidence, claims, deadlines and counters.

The migration refuses destructive rollback. This is separate from the three existing
WhatsApp job kinds; no generic workflow/Attention Item table or cross-provider jobs exist.

`app::compose` returns router, shared WAHA client and shared Google client. Constructors
spawn no tasks. `main` owns an independent Google loop at **500ms**, calling
`google_calendar::worker::run_once`; WAHA's 120-second sender cannot stall it. Graceful
shutdown cancels workers but never erases irreversible dispatch/cleanup evidence.

Refresh ownership is persisted and single-flight across processes. Dispatch is committed
before HTTP; generation/version/claim CAS prevents stale refresh overwrites. A definite
transient response is unavailable, not revoked. `invalid_grant` or persistent Calendar
401 requires reconnect. One coordinated refresh follows a Calendar 401; event insert is
never replayed. An expired dispatched claim or lost/malformed refresh response requires
reconnect with **rotation uncertainty**, not a false revocation claim. No blind refresh
replay assumes Google rotation overlap. Same-generation omitted refresh token retains
its old value; an explicitly supplied null is not omission. Supplied null, empty or
malformed replacements require reconnect for rotation uncertainty; a valid replacement
is saved atomically.

Worker-triggered refresh also checks the live probe claim, authorization context and
actual persisted phase under the connection → probe lock order. Both refresh fences
and the confirmed final COMMIT must leave strictly more than eight seconds in the
claim and phase budgets. Expired observation transitions locally to its independent
cleanup budget before requesting credentials. A valid rotation response already sent
is still saved under the credential generation/version/refresh-claim fences even if
the probe phase expires; the subsequent Calendar request has its own last fence.

## Selection, proof and cleanup are separate facts

CalendarList is always traversed fresh and complete, including hidden entries and all
pages. Deleted entries are excluded; repeated page tokens, duplicate/conflicting IDs,
limits or malformed responses fail closed. Only `owner` is a Booking candidate;
`writer`, `writerWithoutPrivateAccess`, `reader` and `freeBusyReader` do not establish
Booking eligibility. Secondary calendars are not excluded. Calendar metadata must never
be labeled a verified Google email/account identity.

Blocking choices are explicitly zero–100 (no automatically checked Booking Calendar).
Free/busy uses a finite 24-hour window in chunks of at most 50. Every selected calendar
must have a present, error-free result and valid intervals; no event-detail API is used
for Blocking Calendars. Selection saves compare generation/version and clear prior
proof. A provider outage retains known saved labels as unavailable, never as fresh;
storage/decryption failure suppresses stale data and actions. Dashboard Google cleanup
loading is independent of the Contact/Followup queue and its pagination.

A probe creates a transparent/private five-minute test event with no attendees,
recurrence or default reminders, `sendUpdates=none`, a 128-bit random hex event ID,
unique conference request ID and exact purpose/deployment/random-probe private markers.
This remains **a real event**; Google does not guarantee zero notifications. Metadata
about Meet support is advisory, never a successful check.

The lifecycle is `prepared → create_committed → observing → cleanup → complete/attention`.
Confirmed create commitment precedes **one actual insert invocation at most**. A failed or
ambiguous database COMMIT forbids sending. After crash/timeout/409, bounded GETs use only
the stored event ID; no reinsertion, new ID, summary search or blind deletion follows.
Caller IDs reduce duplicate risk but are not an exactly-once provider guarantee.
A lost creation response can still be cleaned up safely; this implementation
conservatively does not claim demonstrated create permission without its own valid
successful insert response.

Video success requires a matching conference request ID, `success`, `hangoutsMeet`, and
exactly one actual video entryPoint. Its URI must be HTTPS on exact `meet.google.com`,
without userinfo, nondefault port, query, fragment, controls, normalization tricks or
unsupported path. Path is `[a-z]+-[a-z]+-[a-z]+`, not a fixed 3-4-3 length. The actual URI
is validated but never synthesized, rewritten, fetched, logged, persisted or displayed.
A failed conference may still prove create/read/delete event permissions after cleanup;
whole insert rejection leaves those permissions unverified, not a blanket claim that
all future in-person operations are impossible.

Cleanup re-reads the exact original target and verifies every marker plus a fresh strong
quoted ETag before `If-Match` DELETE. Wildcards/malformed/weak ETags cannot authorize
unconditional deletion. A 412 causes fresh revalidation, never force delete. An empty
successful 2xx acknowledgement, or documented `410/deleted` for a known own target, can
complete cleanup. A known-own cancelled tombstone is treated narrowly. **404 remains
ambiguous** because inaccessible calendars can also return it. Mismatched markers never
authorize deletion. Deleting an event does not promise immediate destruction of its Meet
space. Tombstones resolve cleanup only: demonstrated event permissions require an
acknowledged matching insert plus a fresh own-event GET and successful empty 2xx DELETE.
Neither an external deletion nor a cleanup-only retry can supply missing permission
proof or enable video capability.

A DELETE 401 consumes one durable corrective-auth attempt before refresh, across worker
claims and restarts. The next DELETE 401 requires reconnect using the existing credential
generation/token-version fence; it cannot invalidate a newer grant. A transient corrective
refresh failure does not restore that attempt. Every permitted DELETE retry still requires
a new own-event GET and strong ETag. Only a new explicit cleanup-only request resets this
bounded authentication attempt; the original unresolved target remains retained.

Only current generation + selection + current pointer can publish capability, and only
after demonstrated event operations, actual Meet success and satisfactory cleanup.
Exhausted old cleanup releases the active slot and remains separately visible; a later
successful capability can coexist with an old warning. Reconnect never automatically
uses a new grant for an old target. Explicit cleanup-only retry revalidates target
permission/markers/version under the current grant and cannot publish capability.

## Hard bounds

- HTTP connect **2s**; complete response, including streamed body, **5s**.
- Token/error **64KiB**; event/freebusy **256KiB**; CalendarList page **512KiB**.
- List **250/page**, **10 pages**, **2500 entries**, **2MiB aggregate**; no partial list.
- Provider-dependent interactive route **12s**; global route timeout remains **15s**.
- Raw callback query **8KiB**; Google forms **64KiB**; duplicate decoded singleton fields
  fail closed, repeated Blocking Calendar fields parsed explicitly.
- Probe **10 starts/rolling hour**, one active check. Observation **12 reads/120s**,
  exponential 1/2/4/8/10-second delay plus jitter. Retry-After is honored only within
  remaining bounded work; a longer instruction ends observations or retains attention.
- Cleanup has independent **5-minute / 12 HTTP reservation** budget, including reads,
  metadata and possible OAuth refresh. An unused refresh reservation is conservatively
  spent rather than allowing crash/retry to exceed the budget.
- Claims **30s**; no HTTP dispatch without at least its **5s HTTP + 3s result-transaction**
  budget remaining. SQL statement **3s**, lock **2s**; result transactions are also
  wall-clock bounded to 3s. A database fence cannot cancel external work already sent.

## Evidence and remaining operational gates

Tests use the full native authenticated router, generated schema in disposable real
Postgres, controlled Clock, reconnected pools, actual worker `run_once`, synthetic HTTP
and local PostgreSQL wire faults that drop or delay a **real successful COMMIT acknowledgement**.
Delayed confirmed acknowledgements and real row-lock waits exercise phase/claim expiry
before dispatch, including worker-triggered refresh.
They cover native scopes/PKCE/state, callback rejection/envelope/logging, grant/refresh
fencing, complete-list/freebusy selection, single-dispatch/crash recovery, actual URI
validation, conditional cleanup, retained warning + later success, limits and deadlines.
No required test silently skips itself; ignored database tests must explicitly run.
The full suite still includes the inherited actual 120-second sender timeout.

Local commands (supply only the dedicated fixture `TEST_DATABASE_URL`):

```sh
cargo test --locked -- --include-ignored --test-threads=4
cargo test --locked
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
cargo build --locked --bins
```

### Historical #9 local acceptance record — 2026-10-14

Parent independently passed **201 tests, zero failures/ignores**, including all 146
inherited tests and the actual 120-second synthetic sender timeout. The ordinary run
passed 34; its 167 explicitly ignored tests ran in the full invocation. Formatting,
strict all-target Clippy, binary builds and tracked/untracked implementation whitespace
passed. Independent source reviews cleared the OAuth fixes and all four probe findings:
actual DELETE evidence, paused committed cleanup visibility, phase/claim dispatch budgets,
and the durable single DELETE-401 correction. The final database fence cannot freeze
ownership atomically through socket dispatch; elapsed-time checks do not claim that.

Fresh native run 5 used the validated immutable binaries, actual CLI/PTTY enrollment,
real Postgres and process restarts/SIGKILL, and native browser forms over verified private
HTTPS. It exercised session/Origin/CSRF negatives, Connect/callback/immediate selection,
exact scopes/S256, encrypted persistence, one-insert recovery, conditional cleanup,
ambiguous404 attention alongside later success, invalid_grant/reconnect/generation
isolation, explicit cleanup-only and logout revocation. The synthetic provider recorded
**59 calls, zero assertion errors, three inserts for three distinct IDs**. Eight actual
1280/360px captures and sensitive-value scans passed. ThinkPad valid-TLS health passed;
the full browser ran confined on this host, not on ThinkPad.

Both application/provider and the entire Node/Chromium tree were OS-network-isolated.
The browser's only external transport was a fixed raw-TLS bridge to the task's private
HTTPS app; there was no host-Node fallback, TLS/CSP bypass or application-response
interception. CDP supplied synthetic consent only, not the egress security boundary.
The real 30-second claim lease expired naturally. Only observation/cleanup/due timestamps
on the disposable uncertain probe were aged; production wall-clock budgets were not
waited, and outcomes/evidence/claims/token expiry were not fabricated.

Parent repeated the read-only cleanup/provenance audit: exact container/database,
recorded processes/namespaces/mounts/private roots/sockets and task ports were absent;
129 source hashes and the fixed binaries matched. Existing Postgres, baseline routes,
DNS/trust and historical evidence were preserved. A subsequent unrelated Serve route
on port18134 was observed and left untouched; current Serve was not globally identical
to the earlier baseline. The test HTTPS endpoint is closed, not a deliverable preview.

**Earlier incident remains:** run 3 escaped the page-only interception and reached real
Google's `invalid_client` error page using synthetic authorization parameters. Exact live
request count is unknown. No real account credentials, consent, token exchange or Calendar
event was observed. This violated the no-live-provider boundary and was disclosed; user
continuation and the independently reviewed whole-browser OS-confinement correction
preceded run 5. Later safe evidence does not erase the incident. No unexpected live-provider
contact was observed in run 5.

At the #9 checkpoint, older #4–#8 browser flags were **not rerun**: their historical results were retained, and the inherited automated tests plus overlapping CLI/auth/settings/restart/logout checks were separate evidence. Do not launch those host-network browser scripts unchanged. The later full-product native rehearsal for #27 ran with the approved OS confinement and is summarized above; it is synthetic local application evidence, not live Google acceptance.

Detailed local artifacts (not committed fixtures): `/tmp/medical-issue9-independent-*.log`,
`/tmp/medical-issue9-probe-fix-review.md`,
`/tmp/medical-issue9-runtime/native-run-5-20261014T201733Z/`,
`/tmp/medical-issue9-native5-parent-cleanup.json`, and
`/tmp/medical-issue9-native5-parent-serve-diff.json`.
The earlier 85 runtime files were hash-verified into
`/tmp/medical-issue9-pre-native5-archive-20261014T201733Z/` before overwrite.

These are application results, not evidence that Google accepted a real client/account.
Issue #9 remains open for delivery/review and its separate operational gates; no commit,
push or deployment occurred. Still required with explicit operator authorization:

- External / In production Web client, Calendar API enablement, exact scopes and callback
  registration, private valid TLS/browser reachability, and Web-client S256 interoperability.
- Applicable personal-use/verification policy and exact `*.ts.net` domain/redirect
  acceptance. **In production is not verification** or proof of a personal-use exception.
- Unverified-app warning frequency is not guaranteed. Calendar-scope External Testing
  grants have the seven-day limitation. The 100 listed test users, unverified lifetime
  project user cap and 100 refresh tokens/account/client are different limits.
- Actual approved-scope manager-calendar/freebusy access and one expressly authorized
  real create/read/Meet/delete check, preserving any unresolved cleanup evidence.

Live Google/WAHA calls, Console changes, production key provisioning, deployment, public
tunnels, patient data and real event mutation are excluded from routine validation.
The earlier boundary violation is explicitly recorded above, not treated as authorized.
