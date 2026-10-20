# Doctor Number verification — issue #8 application record

One Practice, one Doctor, existing `default` GOWS session. This is a local application
implementation, not live WhatsApp sender evidence or release acceptance. Issue #8 remains
open for delivery/review and the relevant operator/provider gates; independent local
application validation is complete.
No live provider request, pairing, reconfiguration, cutover, real enrollment, public
exposure, commit or push is part of this implementation.

## Current Doctor Number integration — approved issues #10–27

The persisted verified Doctor Number now serves two application purposes: its current resolved sender can ask for a read-only agenda, and appointment/payment work can address Doctor notifications to the current verified binding. Recognition never authorizes record changes. A phone that does not resolve to the current binding remains an ordinary Contact; historical LID aliases alone do not grant Doctor privileges.

Agenda responses read application Appointment state only. They do not book, reschedule, cancel, verify payment, resolve Attention Items or change records. Notifications use the shared durable outbox and recheck the current Doctor binding at dispatch; Appointment revision, retention and Takeover fences prevent stale or misdirected work. During a Contact's Takeover, conversational replies stay suppressed while essential operational notices remain eligible. Outbound messaging and automatic Takeover remain OFF by default. Sending a notification or receiving a transport acknowledgement does not prove delivery, attendance or an Appointment decision.

Full local verification on 2026-10-16 passed 245 tests (244 library + 1 CLI), zero failed or ignored. Synthetic full-router/real-Postgres tests cover concurrent booking, explicit natural-language choice, under-review rescheduling, true synthetic Google Meet/recovery, reminders, attendance, cancellation/manual-refund idempotence, authorization/CSRF and retention races. The shared native private-HTTPS/OS-confined Chromium rehearsal used synthetic Doctor enrollment and password+TOTP login and verified Appointment booking/rescheduling plus a Doctor's explicit payment decision; with Google disabled it displayed `confirmed` / `verified` / `noMeet`. This is not a native Doctor-agenda/notification delivery claim and is not live WhatsApp sender evidence.

Real Practice Number pairing, signed sender/`me` evidence from an authorized Doctor phone, provider notification delivery, WAHA session/media restore, and ordinary public privacy-notice reachability remain unaccepted operator gates. The public notice and retention policy remain provisional pending legal/consent review.

## Protected native flow

- `GET /settings/doctor-number` reads persisted binding/challenge and fresh provider status
  only after the shared Doctor authorization middleware. GET never issues, replaces,
  consumes or cleans up a challenge. It never accepts query-derived success claims.
- `POST /settings/doctor-number/challenge` requires the existing opaque session, exact
  Origin and form CSRF token. The shared client reads status before opening a transaction:
  WORKING and a numeric Practice phone are required. Successful issuance redirects with
  `303` to the plain GET. Failed prerequisite checks do not replace a challenge.
- Only the exact `/settings/doctor-number` path joins the existing login return allowlist.
  Both routes retain no-store, CSP and `Referrer-Policy: same-origin`. No code appears in
  a URL, redirect, cookie, external link, QR or automatic outbound message.
- Native refresh shows authoritative stored proof, not a delivery claim. A pending or
  failed replacement preserves the old Doctor Number. There is no unlink action. Active
  challenges always show their saved Practice destination, even if current status changes.
  Provider unavailability does not hide a valid saved binding; storage/decryption errors
  suppress all stale binding/code/action display. Expired ciphertext is never decrypted.

## Authentication controls, not Appointment policy

Six ASCII decimal digits are sampled without modulo bias from OS randomness, preserving
leading zeros. Only surrounding whitespace is trimmed. Validity is **600 seconds**;
processing time after locks and immediately before consumption must be strictly earlier
than expiry. Timely arrival does not rescue expired processing.

Five eligible incorrect guesses exhaust the current challenge globally. An eligible guess
is genuine six-digit raw text with current resolved sender, matching numeric signed `me`,
fresh timestamps, trusted wrote-first/peer/original-target evidence and no identity conflict.
Ineligible inputs, media captions, duplicate callbacks and unresolved senders spend no
attempts. Another Contact can exhaust the challenge: this is a deliberate availability
trade-off for short-code guess resistance. Ordinary replies never reveal challenge state,
remaining attempts or submitted codes.

Issuance allows at most one success per 60 seconds and five per rolling hour, persisted
across sessions/restarts. A protected 429 includes Retry-After. Failed status checks,
allocation failures and rolled-back transactions do not spend successful-issuance history.

## Sender and Practice evidence

Redemption uses **this inbox row's** authenticated `resolved_phone_identity`: numeric
`@c.us` or the current message's `SenderAlt`, normalized from `@s.whatsapp.net` as before.
Typed numbers, LID digits and historical aliases alone never resolve the current sender.

The signed envelope also needs numeric phone-form `me.id`, matching the WORKING Practice
snapshot captured at issuance and different from the sender. Missing, malformed or LID-only
`me` leaves ordinary conversation behavior intact; it does not reject general intake.
The frozen source contract cites WAHA commit
[`90ba445a0e765701c8910f0fac08176250c3e0fd`, `src/core/abc/manager.abc.ts:246–268`](https://github.com/devlikeapro/waha/blob/90ba445a0e765701c8910f0fac08176250c3e0fd/src/core/abc/manager.abc.ts#L246-L268):
the manager populates envelope `me` via `session.getSessionMeInfo()`; the DTO makes it
optional. This is pinned-source evidence, **not observed live sender/me evidence**.
Changing the Practice account requires a new challenge; no automatic cutover is performed.

Phone equality is exact except precisely `52` + ten digits and `521` + those same ten
digits. There is no suffix matching, generic prefix removal, or Contact-history merge.
The binding retains exact source phone, original chat destination, Contact, inbox proof,
verification time and evidenced Practice phone. Equivalent recognition does not rewrite
the original notification destination or itself authorize sending.

## Persistence, crypto and ordering

The additive irreversible migration adds `doctor_number_state` (one empty state row,
not a credential seed) and `doctor_number_challenges`, and extends the existing outbox
CHECK for three templates. No job kind, worker, sender or dependency is added.

Stable `DOCTOR_TOTP_KEY` supplies distinct HMAC-derived encryption and fingerprint keys.
AES-256-GCM uses a fresh nonce and challenge-identity/version-specific associated data,
separate from TOTP's purpose. Keyed fingerprints are compared in constant time. Unique
persisted fingerprints prevent recycling previously issued values; allocation tries at
most 32 candidates before failing closed without replacing old state. Consumption,
supersession and guess exhaustion clear ciphertext and nonce. Expiry cleanup is lazy in
mutating conversation work; there is no cleanup worker.

**Keep the stable key and fingerprint history together.** Offline maintenance
rotation is implemented (`db rotate-doctor-key`, operator-only; see README
"Offline maintenance key rotation"): the stable fingerprint subkey is stored
only encrypted in `doctor_number_state` (48-byte ciphertext + 12-byte nonce,
migration `m202610200001_doctor_number_fingerprint_key`; NULL/NULL means the
pre-first-rotation derivation) and re-wrapped unchanged with the same TOTP
secret in one transaction, so lifetime fingerprint non-reuse survives any
number of rotations. Active challenges are retired by rotation. Rotation is
not compromise remediation, and a lost Doctor key without its matching
escrow/backup remains unrecoverable — there is no reset or backdoor.
**Not every database copy of a submitted code is encrypted:** existing raw inbox bytes
and prepared body necessarily retain the incoming text. Protect the database and backups;
retention remains #25. Never log codes, message bodies, keys, nonces or provider errors.

The existing PostgreSQL `arrived_at` timestamp now receives one sampled `Clock.now_millis()`
value; no column or old row is rewritten. Preparation eligibility derives floor-seconds
from that same sample, preserving existing scheduling. Duplicate delivery preserves the
original arrival and bytes. Redemption needs arrival strictly after issuance milliseconds;
same-millisecond arrival fails closed. Provider seconds earlier than floor(issuance/1000)
are rejected. Provider seconds alone cannot establish ordering within one second.

Conversation jobs retain intake ordering and sorted identity locks. Lock order is job →
sorted aliases → sorted Contacts → Doctor Number singleton → challenge → new reply/job.
Issuance locks singleton then challenge, never Contacts. Replacement never locks the
previous binding's Contact. No SQL lock is held across provider HTTP. The same narrow
argument-based dispatcher context predicate checks incoming ID, trusted peer, original
target and wrote-first inbox proof; authorization logic is not duplicated.

Binding, challenge consumption, one durable templated response, processed marker and job
success share the conversation transaction. Enqueue or commit failure rolls back the
whole transition. Independent chats and concurrent issuance serialize on the singleton.

## Read-only recognition and unchanged sending

Recognized Doctor input takes precedence over ordinary option 5. It never changes Practice,
Service, Followup, Appointment or payment records, resolves prior Followups, or fabricates
an agenda response. Recognition needs current resolved phone and trusted context, but no
crypto key merely to compare a persisted binding. Unresolved or conflicting inputs stay
ordinary. Ignored content remains ignored; audio never redeems.

`doctor_verified`, `doctor_read_only` and `doctor_audio` templates share the existing
conditional provisional privacy prefix, then honest scope and the canonical authenticated
dashboard link, **without the ordinary Contact menu**. Audio gets the honest text fallback;
unknown media is not claimed to have been interpreted. All replies use the existing outbox,
source uniqueness, pacing, cap, dispatch/ack fencing and no-resend boundary. Default-OFF
still makes zero ID/seen/typing/send calls (authorized status reads are separate).

## Verification boundaries and remaining gates

Tests use the complete Axum router with actual password/TOTP sessions and form/HMAC
boundaries, real isolated Postgres, controlled clocks and production preparation,
conversation and sender seams. They cover auth rejection before provider calls, GET safety,
limits/restart, leading zeros and encrypted recovery, tamper failure, bounded collision
allocation, precision/dedup/pre-issuance input, numeric and LID/current-alt evidence,
historical-alias denial, strict Mexican near misses, Practice-account mismatch,
wrong/exact-expiry/fifth-guess/replay, identity conflicts, read-only/media routing,
old-binding preservation, enqueue/deferred-commit rollback and existing sender pacing.
Deterministic PostgreSQL lock gates exercise two-chat consumption, both issue-versus-redeem
orders, expiry during lock wait and absence of prior-Contact locking. Synthetic enabled
sending permits exactly one verification reply POST; default-OFF permits none.

Independent source review found **no must-fix correctness/security findings**. It identified
one nonblocking coverage gap: a missing key had only been exercised after consumption.
Parent added a passing full-router/real-PG regression with an active replacement challenge:
correct input without the key cannot change binding, challenge, failures or ciphertext;
existing Doctor recognition and ordinary intake continue. Restoring the same key preserves
the challenge, but only a fresh valid message can redeem it, never the processed input.

Final independent validation passed **146 tests total** (145 library + 1 database CLI),
retaining all 123 prior tests and adding 23: 22 real-Postgres regressions and one crypto test.
The include-ignored suite passed with zero failures/skips in 126.80 seconds, including the
actual 120-second synthetic send deadline. The ordinary suite passed 32 and explicitly
ignored 114 (113 real-Postgres tests plus that deadline); it is not full acceptance.
Formatting, strict all-target Clippy, binary builds, and tracked/untracked implementation-file
whitespace checks passed. These logs are local artifacts, not portable release records.

| Independent check | Local log |
| --- | --- |
| Final full include-ignored suite | `/tmp/medical-issue8-independent-tests.log` |
| Final ordinary suite | `/tmp/medical-issue8-independent-unit.log` |
| Formatting / strict Clippy / binary build | `/tmp/medical-issue8-independent-{fmt,clippy,build}.log` |
| Tracked and new-file whitespace | `/tmp/medical-issue8-independent-diffcheck.log` |
| Active challenge with missing key | `/tmp/medical-issue8-missing-key-regression.log` |
| Native HTTPS / real process restarts | `/tmp/medical-issue8-independent-browser.log` |
| Prior auth/settings/intake regression | `/tmp/medical-issue8-prior-browser-regression.log` |
| Prior outbox/crash regression | `/tmp/medical-issue8-outbox-browser-regression.log` |
| Fractional arrival RED/GREEN | `/tmp/medical-issue8-arrival-{red,green}.log` |
| Protected route RED/GREEN | `/tmp/medical-issue8-protected-{red,green}.log` |
| Atomic binding RED/GREEN | `/tmp/medical-issue8-binding-{red,green}.log` |
| Strict short-number alias RED/GREEN | `/tmp/medical-issue8-phone-{red,green}.log` |

The short-number regression caught and corrected a too-permissive fallback for a ten-digit
phone starting with `52`; only the two exact permitted lengths now normalize.

The separate UI worker passed 38 isolated browser cases; independent visual review returned
**ship**, without material fixes. Parent additionally drove the actual app over temporary
private HTTPS with valid TLS, native Chromium forms, synthetic enrollment and a loopback
provider. Authentication/Origin/CSRF rejected unsafe requests before provider access; GET
reload and a real SIGKILL/restart preserved the active encrypted code and session unchanged.
Signed LID/current-SenderAlt/Practice evidence bound the exact wrote-first destination;
consumed-code replay could not rebind, and equivalent 52/521 option 5 stayed read-only.
Default-OFF made zero outgoing operations (protected status reads remained separate).

Native replacement retained the binding and used a new fingerprint. To exercise that action
without waiting a minute, only the disposable fixture's issued/expiry timestamps were aged
together; controlled-clock tests separately establish the exact throttle boundaries. Explicit
synthetic opt-in sent one verification response through the existing paced outbox to the
original LID. Another real process restart preserved binding/destination and pending replacement,
without replay; native logout persisted revocation. Prior #4–#7 HTTPS/crash regressions passed.
ThinkPad-side valid-TLS health passed via existing authorized SSH. Task-owned app/provider,
disposable database/private browser state and temporary HTTPS route were removed, preserving
all pre-existing routes. Pool/router reconnection alone is not a process-restart claim, and
none of this proves a real provider restart or live sender/delivery.

Outstanding live gates include
real authenticated callback sender/`me` evidence, human-controlled distinct phones, WAHA
pairing/Postgres-session recreation proof, private callback routing, legal/consent approval
and ordinary Contact privacy-URL reachability. See [WAHA gates](waha.md) and [outbox limitations](conversation-outbox.md).
At the original #8 checkpoint, Agenda (#24), payment notifications (#18), Takeover and retention (#25) were outside that slice; the full #10–27 application now implements the read-only agenda, binding-fenced notifications, Contact-scoped Takeover and retention described above. Manual resend remains unavailable; a committed uncertain text is not replayed. The real provider gates above remain open.
