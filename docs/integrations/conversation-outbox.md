# Contact conversation and durable outbox — issue #7 application record

Scope: one Practice, WAHA `default`, GOWS. This is **local implementation with synthetic
provider evidence**, not live acceptance, deployment, legal approval or production readiness.
[The #6 live gates](waha.md#postgres-storage-and-operator-blocked-live-gates) remain open.
Transport facts come from the [version-pinned research](waha-messaging.md), not another engine.

**Subsequent #8 integration:** [Doctor Number verification](doctor-number.md) joins the
existing conversation transaction after ordered identity locks. Current verified Doctor
input bypasses ordinary Followup/menu handling for honest read-only/dashboard templates,
sharing privacy/outbox controls. No worker, job kind, sender policy or delivery claim changes.
The original #7 validation record below remains historical.

## Current integrated application — approved issues #10–27

The former menu-only checkpoint is superseded. The local application now books, reschedules and cancels Appointments through deterministic WhatsApp flows. One Contact may have at most three Open Appointments; each Appointment keeps stable identity and transactional Hold/revision fences, with Service price/duration, timezone, bank and buffer snapshots. Rescheduling preserves the original payment deadline. The application handles Receipt upload/copy, protected Doctor review, explicit verification/rejection and Late Receipt decisions, and supplies Contact inbox/history plus Appointment history. Selected Google availability, Appointment sync/Meet, reminders, Doctor notifications and recovery run through durable workers. A paid cancellation raises refund attention; the Doctor's native refund-resolution action records an external/manual outcome only.

All outbound purposes share the revision- and Doctor-binding-fenced outbox: `conversation`, `status`, `reminder`, `doctor_notification`, and `payment_details`. Appointment-linked work rechecks revision and state before dispatch; notifications recheck the current verified Doctor binding; retention and Contact-scoped Takeover are also dispatch fences. During Takeover conversational output is silent, while essential operational notices remain eligible. Outbound sending and automatic Takeover default OFF. An uncertain committed text is never automatically resent; acknowledgements are evidence of provider status, not Contact attendance or guaranteed delivery.

Claude is optional and read-only: the fixed production endpoint is used only with a complete `CLAUDE_API_KEY` and explicit `CLAUDE_MODEL`; no model is defaulted. It parses preferences into a closed set but cannot decide availability, payment, Appointment state or any mutation. The application validates choices and renders factual messages. Provider recovery does not erase WAHA's upstream in-memory webhook-retry gap.

Retention deletes associated message text/media only at the 90-day lifecycle boundary after every linked Appointment is terminal and the association graph is rechecked. Structured Appointment facts/history remain for at least five years. No destructive purge after five years is authorized while legal disposition is unresolved; unassociated evidence remains visible for attention without an invented deadline. The privacy notice remains **PROVISIONAL**; legal/consent and ordinary public Contact reachability are not approved.

Final local verification on 2026-10-19 passed 245 tests (244 library + 1 CLI),
zero failed or ignored, plus formatting and strict Clippy. Synthetic full-router,
Postgres and provider tests cover concurrent booking, explicit natural-language
choice, under-review rescheduling, Calendar/Meet recovery, reminders, attendance,
manual-refund idempotence, authorization and retention races.

The actual release-image native run completed signed booking, protected Receipt
desktop/mobile review and a 68-byte browser download, explicit Doctor payment
verification, native cancellation/manual-refund resolution, logout access denial
and the public provisional notice. No Meet was invented with Google disabled,
and the default-OFF runtime made zero provider POSTs or irreversible dispatch
commitments. The [canonical evidence](../../README.md#changelog-and-evidence)
records the image/CLI, TLS, restore and cleanup scope and remaining operator gates.
These are local synthetic results, not live WAHA/Google/Claude or release acceptance.

The remaining sections preserve the historical #7 application slice. Statements below that Appointment/Receipt journeys, Doctor agenda, retention, or application recovery were unavailable describe that earlier checkpoint, not the current full application. #7 outbox safety and local test evidence remain time-specific evidence; live provider/legal gates remain open.

## Operator policy and prerequisites

- `WAHA_OUTBOUND_ENABLED=false` by default. While false there are **zero** generated-ID,
  seen, typing or text requests. Authenticated intake, preparation, conversation/Followups,
  logical enqueueing and signed acknowledgement processing continue.
- `true` requires complete WAHA configuration and canonical HTTPS `PUBLIC_ORIGIN` at
  startup. Every sender phase also checks its configuration; configuration and provider
  client absence preserve pending work rather than consume retries. Use consistent
  configuration in every worker process. Change the operator environment and restart
  consistently; this flag is not a live per-Contact pause switch, Takeover or cancellation
  of provider work already in flight. Pairing, WORKING and settings saves never opt in.
- `WAHA_CONTACT_REPLIES_PER_HOUR` defaults to **20**, integer **1–1000**. The range is an
  implementation validation bound, not a WAHA quota. Count every irreversible dispatch
  commitment in the preceding rolling hour, including rejected/uncertain sends and Doctor
  resolution notices. No Practice-wide quota was invented. Eligibility survives restart.
- Typing duration is sampled uniformly once with OS randomness and persisted: 2000ms
  through `min(6000, 2000 + character_count * 1000 / 80)`, inclusive. Templates stay
  deterministic. These user-approved settings differ from WAHA's 30–60-second/four-per-hour
  examples; neither is a ban-immunity or deliverability guarantee.

## Conversation, privacy and Doctor action

One incoming message produces at most one combined logical response. It uses this menu:

```text
1 Agendar
2 Cambiar cita
3 Cancelar
4 Enviar comprobante
5 Hablar con el doctor
```

Only trimmed exact digits select options. Options 1–4 explicitly state that those journeys
are unavailable and offer 5; they do not request uploads or pretend to modify Appointments.
Option 5 creates/reuses one open Followup Request. Its historical acknowledgement remains
true even if the Doctor resolves before delivery. Doctor resolution locks Contact then
Followup, and only the open-to-resolved winner queues one distinct notification. Repeated
native POSTs return 303 without another notification. It does not assert personal contact,
attendance, payment or Takeover. The route is inside the existing Doctor session/Origin/CSRF
boundary and does not expand the login return allowlist or weaken `Referrer-Policy`.
Resolution redirects to the authoritative dashboard; a query parameter never claims that
a resolution was saved.

Classification examines pinned unwrapped `_data.Message` before normalized body/captions.
Known audio or audio document MIME gets “No puedo escuchar audios; por favor escríbame”
and the menu. Unknown/conflicting/missing raw structures receive generic unsupported-input
guidance, never a guessed media subtype or caption command. Stickers/Lottie/reactions and
raw groups are ignored. No media URLs are fetched. Calls remain ignored.

Until successful send or a trusted positive correlated ack proves a prefix-bearing message
was sent, replies include the **PROVISIONAL** privacy URL before administrative prompts.
Queueing, cancellation and uncertainty do not consume that notice. Redundant prefixes on
already-queued replies are permitted. The URL is derived only from validated `PUBLIC_ORIGIN`,
never request Host or a localhost fallback; missing origin keeps conversation work pending.

Public `/aviso-de-privacidad` has restrictive headers and no Doctor controls. It states the
administrative purpose, asks for no clinical information, and explicitly identifies missing
legal responsible-party/contact/ARCO information. It does **not** solicit or establish consent.
Counsel/consent approval and actual ordinary Contact URL reachability remain launch gates.
An existing private Tailscale preview is not reachable by ordinary Contacts. No public
exposure or legal/consent journey was created.

## Identity and atomicity

Contacts represent chats, not Patients. Authenticated incoming rows establish wrote-first
and alias evidence. Original `@c.us`/`@lid` targets are retained; only the documented
`SenderAlt` phone is normalized. Alias advisory locks are acquired in sorted order, then
existing Contacts are row-locked in UUID order. A unique JID cannot migrate between Contacts.
Evidence joining two existing Contacts flags **both** for attention and suppresses outbound
claims; histories are never merged. Acks and HTTP responses create no Contacts or aliases
and never grant Doctor privileges. LID digits are never displayed as a phone number.

Preparation remains the existing `whatsapp::jobs::run_once` seam and selects only
`prepare_inbox`. Its successful transaction inserts `process_conversation`. Conversation
commits identity, one-open Followup transition, immutable rendered outbox, `send_outbox`,
processed marker and job completion together, respecting earlier unprocessed Contact input.
No origin means no fabricated URL and no exhausted attempts. Database failures preserve
pending conversation work. New rows use an additive irreversible migration; existing raw
intake/preparation evidence is not rewritten. Exactly three typed job kinds are supported.
Doctor resolution uses a Followup target, never a fake incoming message.

## Sender state machine and the one-POST boundary

Public seam: `conversation::sender::run_once(&Db, &dyn Clock, &Config, Option<&Client>)`.
It performs one durable phase and never waits in-process for the sampled typing duration.
Main owns two independently bounded loops, database work versus outgoing HTTP, with explicit
abort/await on shutdown. Router constructors spawn nothing. `app::compose` shares one Client
pool with the protected provider UI and sender. There is no DB transaction across HTTP.

1. Claim the eligible FIFO head under the Contact lock with a fresh token and 30-second
   lease. Check identity/wrote-first, serialized incoming peer against trusted aliases,
   active dispatch window and rolling budget. Missing configuration does not fail work.
2. In `queued`, generate/persist an ID with token/phase fencing, then seen and startTyping.
   Seen uses the original serialized incoming ID and its validated peer (which may differ
   from the original send target only when both are trusted aliases). Transport validates
   exact DTOs, bounds and IDs. Save `typing_due_at = Clock.now_millis() + sampled_duration`,
   change to `typing` and release the lease. Crash recovery may repeat seen/typing, not text.
3. Once due, obtain a fresh token, stopTyping outside the transaction, then lock Contact
   and outbox again. Check the current unexpired token/expected phase, enabled/configured
   policy, context, FIFO and budget. Resample the Clock. Commit `uncertain`, provider raw ID,
   irreversible commitment and a **125-second** active dispatch deadline, consuming budget
   and completing the send job. No call is authorized if COMMIT fails or is ambiguous.
4. Check the bounded dispatch permission immediately before one `sendText` call, with
   a 120-second request deadline. The shared HTTP client disables automatic retries,
   redirects and ambient proxies, and keeps TLS verification. No committed item ever
   becomes automatically eligible for another text POST.

Pre-text failures retry after 5/10/20/40 seconds, with five failed attempts total, then
remain failed with sanitized diagnostics and attention. IDs and sampled delays persist.
A complete valid expected-ID/trusted-peer send response records server acknowledgement
(`sent`), not recipient delivery. Narrow exact pre-engine session 422 rejections are failed;
unknown errors/timeouts/resets/truncation/malformed or oversized results remain uncertain.
Even a conclusive text rejection has no automatic retry. Raw provider exceptions and
message bodies/IDs/phone numbers are absent from ordinary logs.

A crash after commitment but before a socket write can lose delivery. This sacrifice avoids
an unproven duplicate-submit guarantee. A 125-second window serializes application dispatch
permissions per Contact; after expiry later **distinct** logical messages may proceed, but
old uncertainty never retries. HTTP cancellation does not cancel WAHA, so this is **not** a
provider-side nonoverlap, eventual ordering, eventual delivery or exactly-once guarantee.
There is no manual resend, provider recovery/backfill or blanket startup lease reset.

The dashboard does not treat a normal in-flight request as problem attention. An uncertain
item becomes visible only at its own persisted dispatch deadline, including after an early
ambiguous HTTP failure; definitive failures and reported ack errors appear immediately.
Filtering happens before attention ordering/pagination. A newer Contact dispatch window
cannot hide an older unresolved item. This is a presentation rule only: it neither changes
durable uncertainty nor authorizes retries or asserts that provider work has stopped.

## Signed acknowledgements

The existing 2MiB raw HMAC boundary runs before JSON/database access. `message.ack` requires
configured `default`/GOWS, outgoing direction, valid matching serialized ID/peer and exact
integer enum -1..4. Invalid recognized acks get 400, authentication failures 401, persistence
failures 503; unrelated directions/groups/engines/sessions are ignored. Minimal authenticated
fields are stored with envelope and semantic deduplication, including early/unmatched evidence.
No full ack payload or `_data` dump is stored.

Projection and bounded reconciliation use candidate lookup followed by Contact → outbox →
evidence lock ordering. Only a committed dispatch with a trusted peer matches. Positive
rank is `max(previous, incoming)`; ERROR is retained independently and late HTTP success
cannot erase READ/PLAYED or error evidence. Positive evidence can establish privacy-prefix
progress; missing acknowledgements never establish failure or attendance. Alias evidence
arriving later can reconcile prior signed evidence without creating aliases from the ack.
The operator must separately authorize a preserved `message.ack` webhook subscription;
no provider configuration was changed.

## Local evidence and remaining gates

Tests use actual full-router HMAC/Doctor requests, isolated real Postgres, controlled Clock,
actual loopback synthetic HTTP and real Postgres gates. Coverage includes:

- First-5/privacy combined reply, truthful menu and media/caption precedence.
- Followup/outbox/job rollback, replay, concurrent resolution, auth/Origin/CSRF and
  combined attention pagination/timezone/read failure states.
- Concurrent evidence-backed aliases, invalid seen peer and zero outbound HTTP while
  disabled or missing prerequisites. A schema-local PostgreSQL gate verifies that crossed
  mappings between two existing Contacts serialize intake, preserve alias/history ownership,
  flag both Contacts and suppress all provider calls; it does not claim simultaneous
  conflicting Contact mutations.
- Persisted typing bounds/deadline, restart budgets including failed sends and Doctor
  notices, two-worker claims, stale lease fencing and CAS before irreversible dispatch.
  An old generator's delayed HTTP 503 cannot change a replacement owner's persisted token,
  lease, ID, eligibility, failures or job state; only the replacement can complete the send.
- Cancellation before typing commit, before wire commit and after text commitment;
  unknown HTTP outcomes remain uncertain without a second POST. A stalled text call
  leaves intake/conversation and protected dashboard responsive.
- Deferred real database COMMIT failure and a test-only loopback Postgres protocol relay
  that drops a **successful** COMMIT response: durable uncertainty exists, yet zero text
  calls occur and restart does not resend. No production pause/debug hook was added.
- Signed early READ, stale DEVICE, ERROR and late HTTP; unmatched alias reconciliation,
  malformed/authentication rejection without effects, safe preflight retry/terminal states.
- Source-correct provider DTO/parser/body limits/empty seen/reset/truncation/redirect tests,
  including an actual 120-second synthetic send deadline. Include ignored tests explicitly.

Final independent validation passed **123 tests total** (122 library + 1 database CLI),
with zero failures or skips under `cargo test --locked -- --include-ignored`: 91 real-Postgres
tests, one actual 120-second synthetic transport deadline, and the 31 ordinary tests.
The ordinary suite passed 31 and explicitly skipped 92; it is not full acceptance.
Formatting, strict all-target Clippy, binary builds and `git diff --check` also passed.
The earlier 79 tests are retained, with legitimate new-kind counts and dashboard wording
updated. Local artifacts (not portable release records):

| Check | Log |
| --- | --- |
| Final full include-ignored suite | `/tmp/medical-issue7-independent-tests.log` |
| Final ordinary suite | `/tmp/medical-issue7-independent-unit.log` |
| Formatting | `/tmp/medical-issue7-independent-fmt.log` |
| Strict all-target Clippy | `/tmp/medical-issue7-independent-clippy.log` |
| Binary build | `/tmp/medical-issue7-independent-build.log` |
| Diff whitespace check | `/tmp/medical-issue7-independent-diffcheck.log` |
| Attention defect RED/GREEN | `/tmp/medical-issue7-attention-{red,green}.log` |
| Crossed identity / stale error regressions | `/tmp/medical-issue7-{conflict,stale-error}-regression.log` |
| Prior auth/settings/intake browser regression | `/tmp/medical-issue7-prior-browser-regression.log` |
| Final native HTTPS, process crash and real callback flow | `/tmp/medical-issue7-independent-browser.log` |

Independent source review found the premature in-flight attention defect described above;
two new regressions failed before the correction and passed afterward. The correction also
removes query-derived success claims. Focused independent re-review found no blockers.
The two additional concurrency regressions close the review's coverage gaps without changing
production dispatch behavior. No remaining must-fix correctness/security findings were identified.

The UI worker separately verified 40 isolated desktop/mobile/native-form cases under
`/tmp/medical-issue7-ui`; independent visual review approved them. Parent then exercised the
actual application via private HTTPS with valid TLS, synthetic credentials, isolated Postgres,
and a loopback provider: native login, provisional public notice, protected Followup resolution,
CSRF rejection, replay, mobile layout and logout. Default-OFF produced zero provider operations.
An enabled synthetic text request was held open while intake and the dashboard remained responsive,
without falsely showing active uncertainty as a problem. Parent killed the actual app process,
confirmed durable uncertainty, aged only the disposable fixture's dispatch window, and restarted:
the first text was never replayed, while distinct replies proceeded. Early signed READ callbacks
before HTTP success and later READ/DEVICE/ERROR preserved monotonic progress and separate error
attention. Session and resolved Followup state survived restart. This is not live delivery proof.

A ThinkPad valid-TLS check passed on an earlier #7 run; SSH to the ThinkPad timed out on the
final rerun, so final client-side reachability is unverified. Host-side private TLS passed.
Task-owned app/provider listeners, disposable database/private browser state and temporary HTTPS
route were removed; existing Tailscale routes were preserved. No live WAHA requests, pairing,
storage cutover, real Doctor enrollment, public Contact reachability test, legal approval,
commit, push or production deployment occurred. Full live/provider/privacy acceptance remains gated.
