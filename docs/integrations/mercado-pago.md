# Mercado Pago Checkout Pro payments (spec)

Status: **accepted and implemented on `feat/mercado-pago`** (2026-11-04; D1–D5 approved as recommended, so the webhook in 4.3 is **not** implemented). Decision record: [ADR 0006](../adr/0006-mercado-pago-online-payment.md). Nothing here has been tested against Mercado Pago; the evidence is the synthetic fixture in `src/modules/mercado_pago/`. This record lists operator requirements and provider facts from Mercado Pago Developers (MX). It does not record acceptance of any account, token, payment, refund or webhook.

Bank transfer, Receipt review and the Doctor's explicit Payment Verification do not change. Mercado Pago is an optional second way to pay a video Appointment. With no token configured the app behaves as it does today without Stripe.

## 1. Account model

| Option | What it needs | Verdict |
| --- | --- | --- |
| **Doctor's own access token** (recommended) | The Doctor creates an application in *Tus integraciones* in their Mercado Pago account and activates production credentials. The operator stores the production access token as `MERCADO_PAGO_ACCESS_TOKEN`. | One seller per deployment (ADR 0001). No platform app, no callback, no token refresh. Money and fees land in the Doctor's account. |
| Mercado Pago OAuth (marketplace) | A platform application owned by Hendrik, client secret, `/oauth/token` authorization-code exchange, refresh-token storage and rotation, and re-authorization handling. | Built for many sellers plus a `marketplace_fee`. Access tokens last 180 days (`expires_in=15552000`), and every refresh also rotates the refresh token. That is extra moving parts and credentials with no v1 benefit. Revisit only if one deployment ever serves several sellers. |

Consequence of the recommended model: the token can do anything the Doctor's account API allows, including refunds. Treat it like `INCEPTION_API_KEY`: app-only provider file, never in source, logs, arguments, screenshots or Doctor-facing pages. It is not shown in the dashboard.

At startup and every 6 hours the worker calls `GET /users/me` and stores the returned user `id` in `mercado_pago_account.user_id`. Every payment must have `collector_id` equal to it. This prevents a test token and a production token, or two Doctors' tokens, from being mixed up. A `401` creates an Attention Item `mercado_pago_unauthorized` and stops creating new preferences. Booking keeps working with bank transfer only.

## 2. Configuration

| Variable | Default | Meaning |
| --- | --- | --- |
| `MERCADO_PAGO_ACCESS_TOKEN` | absent = disabled | Doctor's production (or, in sandbox, test-seller) access token. Printable ASCII, no whitespace, at most 255 bytes, prefix `APP_USR-` (the prefix Mercado Pago documents for both test and production credentials) or legacy `TEST-`. Requires canonical HTTPS `PUBLIC_ORIGIN`, like Stripe did. |
| `MERCADO_PAGO_WEBHOOK_SECRET` | absent = webhook route disabled | Only if decision D2 approves a public webhook. The secret shown in *Tus integraciones → Webhooks*. At least 32 bytes. |
| `MERCADO_PAGO_NOTIFICATION_URL` | absent | Only with D2. Exact public HTTPS URL of the webhook route, sent as the preference `notification_url`. It must differ from `PUBLIC_ORIGIN`, which stays tailnet-private. |

How to obtain them:

1. The Doctor signs in to Mercado Pago Developers with the Practice's Mercado Pago account, opens *Tus integraciones*, creates an application for Checkout Pro and activates production credentials. The production access token is under *Credenciales de producción*.
2. The Doctor passes the token to the operator over the same private channel used for other provider secrets (see D5). The operator writes it to the app-only provider file and restarts the app.
3. Sandbox: the application gets a test seller account automatically. Its test credentials also start with `APP_USR`. Create a test buyer under *Cuentas de prueba* (same country, MX; at most 15 accounts, which cannot be deleted). Use Mercado Pago's documented test cards with the buyer account.

Remove `STRIPE_SECRET_KEY` from `.env.sample`, `deployment/compose.yml` comments and README.

## 3. Data model

One new migration replaces the uncommitted `m202610240001_stripe_connect` (see section 9).

```sql
-- appointment_jobs.kind: replace 'stripe_link' with 'mp_preference','mp_refund'
CREATE TABLE mercado_pago_account (
  id integer PRIMARY KEY CHECK (id=1),
  user_id bigint,            -- from GET /users/me; must equal payment.collector_id
  checked_at bigint, notice text,
  created_at bigint NOT NULL, updated_at bigint NOT NULL);

CREATE TABLE mercado_pago_preferences (
  appointment_id uuid NOT NULL REFERENCES appointments(id),
  revision bigint NOT NULL CHECK (revision > 0),
  external_reference text NOT NULL UNIQUE,   -- 'ma1:<appointment uuid>:<revision>'
  preference_id text UNIQUE, url text,       -- init_point
  amount_centavos bigint NOT NULL CHECK (amount_centavos > 0),
  expires_at bigint NOT NULL,                -- = Receipt Deadline
  closed_at bigint, next_check_at bigint,
  outcome text NOT NULL DEFAULT 'pending'
    CHECK (outcome IN ('pending','active','closing','closed')),
  created_at bigint NOT NULL,
  PRIMARY KEY (appointment_id, revision),
  CHECK ((preference_id IS NULL)=(url IS NULL)));

CREATE TABLE mercado_pago_payments (
  payment_id bigint PRIMARY KEY,             -- Mercado Pago payment id; one row per payment ever seen approved
  appointment_id uuid NOT NULL, revision bigint NOT NULL,
  amount_centavos bigint NOT NULL,
  outcome text NOT NULL CHECK (outcome IN ('confirmed','late','duplicate','mismatch')),
  provider_status text NOT NULL,             -- last status read: approved, refunded, charged_back, ...
  refund_state text NOT NULL DEFAULT 'none'
    CHECK (refund_state IN ('none','requested','refunded','failed')),
  refund_id bigint, next_check_at bigint,
  first_seen_at bigint NOT NULL, updated_at bigint NOT NULL,
  FOREIGN KEY (appointment_id, revision) REFERENCES mercado_pago_preferences(appointment_id, revision));
```

Attention kinds: rename `stripe_payment_late` / `stripe_payment_duplicate` to `online_payment_late` / `online_payment_duplicate`. Add `online_payment_reversed` (refund or chargeback not started by the app) and `mercado_pago_unauthorized`. `refund_due` stays as it is.

History kinds: `payment_verified_mercado_pago`, `mercado_pago_payment_late`, `mercado_pago_refund_requested`, `mercado_pago_refunded`, `mercado_pago_refund_failed`, `mercado_pago_payment_reversed`.

## 4. Flows

### 4.1 Create and send the preference

Same trigger points as the Stripe `schedule_link`: booking a video Appointment while Mercado Pago is configured and the account check is good, and Doctor rejection that opens a new Receipt window (new revision). The booking transaction inserts the `pending` row and an `mp_preference` job. After commit the job calls:

`POST https://api.mercadopago.com/checkout/preferences`

```json
{
  "items": [{"id": "consulta-video", "title": "Consulta por video 12/11/2026 10:00",
             "quantity": 1, "currency_id": "MXN", "unit_price": 500.00}],
  "external_reference": "ma1:<appointment uuid>:<revision>",
  "expires": true,
  "expiration_date_from": "<now, ISO 8601 with offset>",
  "expiration_date_to": "<Receipt Deadline, ISO 8601 with offset>",
  "payment_methods": {
    "excluded_payment_types": [{"id": "ticket"}, {"id": "atm"}, {"id": "bank_transfer"}],
    "installments": 1
  },
  "binary_mode": true,
  "notification_url": "<only with D2>"
}
```

- `unit_price` is serialized from `amount_centavos` with exactly two decimals, never through a float. `installments: 1` keeps `transaction_amount` equal to the price.
- `binary_mode: true` asks Mercado Pago to approve or reject immediately instead of leaving card payments `in_process`. Verify the effect in sandbox.
- No `payer`, phone, name, email or `metadata` with Contact data. No `back_urls`: Mercado Pago's own result page is shown, and a browser return never confirms anything.
- Exact excluded type IDs for MX must be confirmed in sandbox with `GET /v1/payment_methods` before going live. `bank_transfer` may not exist as a type ID in MX.

The job stores `preference_id` and `init_point`, sets `active`, and enqueues a `payment_details` outbox message (`Pague con tarjeta o saldo de Mercado Pago aquí: <url>`) with key `appointment:<id>:<revision>:mercado_pago_link`. It is enqueued only if the Appointment is still `awaiting_payment` with the same revision and deadline. Otherwise the row is closed and nothing is sent.

Retries: transport errors and `5xx`/`429` retry with the existing `appointment_jobs` backoff. A duplicate preference after a lost response is harmless: payments are found by `external_reference`, not by preference ID, and only the stored URL is ever sent. `4xx` other than `429` closes the row and records a notice. Bank transfer instructions were already sent, so the Contact can still pay.

### 4.2 Poll and confirm

Worker loop (replaces `stripe::worker::run_once` in `main.rs`): claim a due preference row `FOR UPDATE SKIP LOCKED`, then

`GET /v1/payments/search?external_reference=<ref>&sort=date_created&criteria=asc&range=date_created&begin_date=<created-1d>&end_date=NOW`

For each result, re-read nothing from the search body that matters for money: call `GET /v1/payments/{id}` and apply the table below. Cadence: every 30 s while `active`. After `expires_at`, one check at +15 min and one at +2 h (Mercado Pago documents up to 2 business hours to credit some methods), then `closed`.

| `status` (from `GET /v1/payments/{id}`) | Action |
| --- | --- |
| `approved`, and `currency_id=MXN`, `transaction_amount`×100 = `amount_centavos` (decimal compare), `external_reference` equal, `collector_id` = stored user id | Insert into `mercado_pago_payments` (`ON CONFLICT DO NOTHING` → idempotent). Lock the Appointment. If it is still `awaiting_payment`, the revision/deadline match and the deadline has not passed: `confirm_appointment_payment` with actor `System`, outcome `confirmed`, WhatsApp "Mercado Pago confirmó su pago…". If Payment is already `verified` (transfer or earlier payment): outcome `duplicate` + `online_payment_duplicate`. Otherwise (expired, cancelled, rescheduled away): outcome `late` + `online_payment_late`. |
| `approved` but any field above differs | Outcome `mismatch`, Attention Item for the Doctor. Never confirm. |
| `pending`, `in_process`, `authorized`, `in_mediation` | No state change. Keep polling until the post-expiry checks. A payment still pending when the Hold expires does not protect the Hold (unlike a timely Receipt). If it is approved later it becomes `late`. |
| `rejected`, `cancelled` | Ignore. The Contact may retry the same preference until it expires. |
| `refunded`, `charged_back` on a payment already recorded | Update `provider_status`. If the app did not request the refund: `online_payment_reversed` Attention Item and history. Payment Verification is **not** reversed automatically; the Doctor decides. |

`transaction_amount` is a JSON number in pesos. Parse it from the raw JSON text with an exact decimal parser (at most two decimals) into centavos. Reject anything else as `mismatch`.

### 4.3 Webhook (only if D2 is approved)

Route `POST /api/v1/webhooks/mercado-pago`, unauthenticated by session, bounded body (16 KiB), enabled only when `MERCADO_PAGO_WEBHOOK_SECRET` is set.

1. Read `x-signature` (`ts=<ts>,v1=<hex>`): split on `,`, then on `=`; trim. Read `x-request-id`. Read `data.id` from the **query string** (Mercado Pago sends `?data.id=…&type=payment`); lowercase it if alphanumeric.
2. Manifest: `id:<data.id>;request-id:<x-request-id>;ts:<ts>;` (omit a part whose value is missing, per Mercado Pago's template).
3. HMAC-SHA256 with the secret, hex, constant-time compare with `v1` (`hmac`, `sha2`, `subtle` are already dependencies). Reject `ts` more than 5 minutes from server time (app policy; Mercado Pago does not mandate one). Failure → `401`, no body detail, no logging of headers.
4. Valid and `type=payment`: insert `(payment_id, received_at)` into a small dedupe queue and answer `200` immediately (Mercado Pago waits 22 s, then retries every 15 minutes). The worker then does `GET /v1/payments/{id}`, finds the preference by `external_reference` and applies the same table. The notification body is never trusted for status or amount.

Even with the webhook, polling stays on as the safety net. The webhook mainly adds post-expiry refunds and chargebacks that polling would only see on its rechecks.

### 4.4 Close a preference early

Like Stripe deactivation. On cancellation, expiry, confirmed payment, or a Doctor rejection that opens a new window, mark the row `closing` and call `PUT /checkout/preferences/{id}` with `expires=true`, `expiration_date_to=<now>`. Then mark it `closed` while keeping the post-expiry rechecks. Rescheduling keeps the same revision's preference and Receipt Deadline, matching current behavior. A failed PUT retries with backoff. If it never succeeds, the native `expiration_date_to` still ends the preference at the Receipt Deadline.

### 4.5 Refund (Doctor-triggered, D4)

Shown on `refund_due`, `online_payment_late` and `online_payment_duplicate` items when the money came through Mercado Pago (`mercado_pago_payments` row with `refund_state='none'`). Button **Reembolsar con Mercado Pago**, a POST with the existing CSRF/session protection:

1. Transaction: `refund_state='requested'`, history `mercado_pago_refund_requested` (actor Doctor), `mp_refund` job.
2. Job: `POST /v1/payments/{payment_id}/refunds` with header `X-Idempotency-Key: ma1-refund-<payment_id>` and body `{"amount": <transaction_amount>}` (explicit full amount). A retry reuses the same key, so a lost response cannot refund twice.
3. Success: store `refund_id`, then `GET /v1/payments/{id}` until `status=refunded` (or `refund_state` reported). Then `refunded`, history `mercado_pago_refunded`, resolve the Attention Item, and send the Contact an outbox message. It does **not** reverse Payment Verification or move the Appointment. This matches today's refund-resolution rule.
4. `4xx` (for example a payment too old to refund, or insufficient balance): `failed`, history `mercado_pago_refund_failed`. The Attention Item stays with the text "Reembolsa desde tu cuenta de Mercado Pago y marca el reembolso como resuelto". The existing **Marcar reembolso como resuelto** still works.

If D4 is rejected, skip 4.5 entirely. The Doctor refunds in Mercado Pago and uses **Marcar reembolso como resuelto**, as with Stripe.

### 4.6 Reconciliation and retries

- All provider calls run in `appointment_jobs` jobs or the poll worker, never inside a request handler or the booking transaction. Existing backoff and `SKIP LOCKED` claiming are reused.
- WhatsApp messages go through the existing outbox with stable keys, so a retried job does not send twice.
- Startup sweep: every non-`closed` preference and every `requested` refund becomes due immediately.
- Confirmed payments are rechecked daily until 7 days after the Appointment start, to catch refunds or chargebacks made outside the app. That is the only chargeback detection without D2.
- The settings page `/settings/mercado-pago` is read-only: configured or not, last `users/me` check, last notice. There is no connect or disconnect action; the operator removes the token. Before removing it the operator should wait for open preferences to expire, because after that the app cannot see payments on them.

## 5. Request and data safety

- Fixed base URL `https://api.mercadopago.com/`; only `#[cfg(test)]` builds may override it. No proxies, no redirects, no client retries; 2 s connect / 5 s request timeouts; `Authorization: Bearer` header marked sensitive. Same as the Stripe client.
- Never log the token, webhook secret, `init_point` URLs, `x-signature`, payer data or full provider responses. Log payment IDs only at debug level.
- `external_reference`, item title and any idempotency key contain the Appointment UUID, revision, consultation type and local time only.

## 6. Code changes (for the implementation phase)

| Area | Change |
| --- | --- |
| `src/modules/mercado_pago/` (new) | `config.rs`, `client.rs` (preferences POST/PUT, payments search/get, refunds, users/me), `store.rs` (`schedule_preference`), `worker.rs`, `http.rs` (settings page, refund action, optional webhook), `views.rs`, `tests.rs`, `worker_tests.rs`. Port the Stripe worker structure, which already solves locking, fencing and deactivation. |
| `src/modules/stripe/` | Delete. |
| Migrations | Delete `m202610240001_stripe_connect.rs` + `stripe_connect.sql`. Add `m202611040001_mercado_pago.rs` (section 3). No explicit guard: SeaORM already refuses to run against a database that applied a migration missing from the list, so a dev database that ran the WIP Stripe migration must be recreated. |
| `appointments/booking.rs`, `store.rs`, `timers.rs`, `payments/actions.rs` | `stripe_payment_links` → `mercado_pago_preferences`; `stripe::store::schedule_link` → `mercado_pago::store::schedule_preference`. |
| `conversation/views.rs`, `conversation/http.rs` | Renamed or new attention kinds and Spanish copy referring to Mercado Pago. |
| `doctor/http.rs` | Remove `/settings/stripe*` routes and the `connect.stripe.com` CSP source; add `/settings/mercado-pago`. |
| `practice/views.rs` | Settings link and copy. |
| `common/config/mod.rs`, `app.rs`, `main.rs`, `parser.rs` tests | `stripe` → `mercado_pago` config, client and worker. |
| Docs | `CONTEXT.md` (Payment Link, Payment Verification), `AGENTS.md`, workflow `SKILL.md`, `decision.md`, `PRODUCT.md`, `README.md`, `.env.sample`, `deployment/compose.yml`. Delete `docs/integrations/stripe.md`; mark ADR 0004 superseded. |

## 7. Test plan (no real Mercado Pago calls)

Same approach as Stripe: a local Axum fixture that impersonates `api.mercadopago.com`, plus the disposable Postgres fixture for `#[ignore]` database tests.

Unit / client (`cargo test --locked`):
- Config: absent token disables; malformed token (whitespace, non-ASCII, wrong prefix, >255 bytes) rejected with a static error that never echoes the value; webhook secret <32 bytes rejected; webhook vars without token rejected.
- Exact request shapes: preference body (amount `500.00` from 50000 centavos, `MXN`, excluded types, `installments=1`, `binary_mode`, `expires`/dates with offset, no payer data), PUT expiry, search query, refund body and `X-Idempotency-Key`, sensitive Authorization header.
- Amount parser: `500`, `500.0`, `500.00` → 50000; `500.001`, `-1`, `1e3`, strings → mismatch.
- Signature: known-vector HMAC; wrong secret, altered `data.id`, altered `request-id`, missing `v1`, stale `ts`, uppercase alphanumeric id → expected results; constant-time compare used.

Database (`cargo test --locked -- --include-ignored`, disposable Postgres only):
- Booking with token → pending row + job; job → preference stored + one outbox message; provider 5xx → retry, no message; 4xx → closed, booking unaffected.
- Approved matching payment → Appointment paid, history actor `System`, payment row; replaying the same payment → no second effect.
- Amount, currency, collector or `external_reference` mismatch → `mismatch` attention, not paid.
- `pending` / `in_process` → no change; `rejected` → no change.
- Approved after Hold expiry → `late` + attention; approved when already verified by transfer → `duplicate` + attention; two approved payments → one confirmed, one duplicate.
- Cancellation, Doctor rejection (new revision → new preference) and deadline → PUT expiry; reschedule keeps the preference.
- Refund: request → job → refunded → attention resolved, verification not reversed; retry reuses the idempotency key; 4xx → failed and attention stays.
- External refund or chargeback on a confirmed payment → `online_payment_reversed`.
- Webhook (if D2): bad signature → 401 and no job; valid → payment checked via fixture GET; forged body with `approved` but provider says `pending` → not paid.
- `401` from `users/me` → `mercado_pago_unauthorized`, no new preferences, bank transfer still offered.

Operator sandbox acceptance (separate, manual; not claimed by any test): test seller token in a staging deployment, test buyer pays with approved and rejected test cards, confirm the excluded payment types are absent in the Checkout Pro UI, confirm `binary_mode` behavior, refund from the dashboard, and (if D2) confirm signed delivery and that `notification_url` notifications carry `x-signature`. Live acceptance with a real small payment and refund is a separate gate approved by Hendrik.

## 8. Provider facts this spec relies on

| Fact | Source |
| --- | --- |
| Webhook header `x-signature: ts=…,v1=…`; manifest `id:[data.id_url];request-id:[x-request-id_header];ts:[ts_header];`; HMAC-SHA256 hex with the application's secret; `data.id` from query params | [Webhooks](https://www.mercadopago.com.mx/developers/es/docs/your-integrations/notifications/webhooks), [Notificaciones (Orders)](https://mercadopago.com.mx/developers/es/docs/order/online-payments/notifications) |
| Reply `200`/`201` within 22 s; otherwise retried every 15 minutes; topic `payment` covers Checkout Pro | [Webhooks](https://www.mercadopago.com.mx/developers/es/docs/your-integrations/notifications/webhooks) |
| Preference validity `expires`, `expiration_date_from`, `expiration_date_to` (ISO 8601) | [Vigencia de preferencias](https://www.mercadopago.com.mx/developers/es/docs/checkout-pro-preferences/additional-settings/term-of-preference) |
| Offline-method expiry `date_of_expiration`; recommend ≥3 days; crediting up to 2 business hours; late payments returned to the payer's Mercado Pago account | [Expiration date](https://mercadopago.com.mx/developers/en/docs/checkout-pro/checkout-customization/preferences/expiration-date) |
| `excluded_payment_types` / `excluded_payment_methods`; `ticket` covers OXXO/PayCash, `atm` covers ATM payments | [Excluir medios de pago](https://www.mercadopago.com.mx/developers/es/docs/checkout-pro/additional-settings/payment-methods), [tipos de pago MX](https://www.mercadopago.com.mx/developers/es/docs/vtex/payments-configuration/checkout-pro/exclude-payment-types-methods) |
| Payment search by `external_reference` (last 12 months; range < 365 days) with `status`, `currency_id`, `transaction_amount`, `collector_id` | [Buscar pagos](https://www.mercadopago.com.mx/developers/es/reference/online-payments/checkout-pro/search-payments/get) |
| Payment statuses and details (`pending`/`waiting_transfer`, `cancelled`/`expired` after 30 days pending, `charged_back`) | [Query results](https://mercadopago.com.mx/developers/en/docs/checkout-api/response-handling/query-results) |
| Refund `POST /v1/payments/{id}/refunds` with `X-Idempotency-Key` and optional `amount`; payments can be too old to refund | [Reembolsos](https://www.mercadopago.com/developers/es/reference/chargebacks/_payments_id_refunds/post) (the MX page could not be fetched on 2026-11-04; re-verify) |
| OAuth tokens: `expires_in=15552000` (180 days), refresh needs `offline_access`, refresh rotates the refresh token | [Renovar Access Token](https://www.mercadopago.com.mx/developers/en/docs/oauth/renewal), [OAuth token](https://mercadopago.com.mx/developers/es/reference/oauth/_oauth_token/post) |
| Marketplace uses OAuth seller tokens and `marketplace_fee` on preferences | [Integrar marketplace](https://www.mercadopago.cl/developers/en/docs/checkout-pro/how-tos/integrate-marketplace) (not the MX page) |
| Test seller created automatically; test token prefix `APP_USR`; up to 15 test accounts, same country, not deletable | [Cuentas de prueba](https://www.mercadopago.com.mx/developers/es/docs/your-integrations/test/accounts) |

Not confirmed from MX docs and must be checked in sandbox before implementation ends: exact MX payment-type IDs (`bank_transfer`?), the effect of `binary_mode` in Checkout Pro, whether per-preference `notification_url` deliveries carry `x-signature`, refund time limits, and Mercado Pago API rate limits for the 30-second poll. Mercado Pago also promotes a newer Orders API. This spec stays on Checkout Pro preferences, which the Checkout Pro docs still use.

## 9. Removing Stripe and existing data

Verified on 2026-11-04 in this repository: `git log --all -- src/modules/stripe src/database/migrations/stripe_connect.sql` returns no commits, and `master`'s `migrations/mod.rs` has no Stripe migration. The Stripe module, its migration and `stripe_connection` / `stripe_payment_links` exist only as uncommitted work. Deployments run from commits through Woodpecker, so no deployed database can contain Stripe tables or Stripe-paid Appointments. **No data migration is needed.** No deployed database was inspected. A dev database that ran the WIP Stripe migration must be recreated; the guard in the new migration makes that explicit.

## 10. Open decisions for Hendrik

| # | Decision | Recommendation |
| --- | --- | --- |
| D1 | Doctor's own access token vs Mercado Pago OAuth (marketplace). | **Own token.** One seller per deployment, no fee, nothing to refresh. |
| D2 | Confirmation by polling only, or also a public webhook route (`MERCADO_PAGO_NOTIFICATION_URL`). That route is an exposure change. | **Polling only for v1.** It needs no exposure change. Add the webhook later only if faster chargeback detection matters; it would be a separate approval of the hostname and route. |
| D3 | Online methods: immediate only (cards + account money), or also OXXO/SPEI. | **Immediate only.** The Receipt window is minutes (default 120); Mercado Pago recommends ≥3 days for cash. Bank transfer already covers SPEI. Revisit only if the Doctor sets windows of several days. |
| D4 | Refunds: a **Reembolsar con Mercado Pago** button (explicit Doctor action, API refund), or keep refunds manual in Mercado Pago as with Stripe. | **Button.** One click, idempotent, Doctor-initiated, with manual fallback. Choosing "manual" removes section 4.5 and the refund job. |
| D5 | Who handles the token: the operator places it in the app-only provider file, or the Doctor pastes it into an encrypted dashboard field. | **Operator places it.** That matches every other provider secret and AGENTS.md (enrollment and deployment are operator-only). A dashboard field would need a new encrypted-secret store and key rotation. |

## 11. Implementation notes (2026-11-04)

- Code: `src/modules/mercado_pago/` (`client.rs`, `worker.rs`, `store.rs`, `http.rs`, `views.rs`), migration `m202611040001_mercado_pago`, worker loop in `main.rs` every 5 s.
- Deviations from the spec text: no migration guard (SeaORM rejects unknown applied migrations); a preference-creation `4xx` closes the row without a stored notice; the refund button lives on the Appointment detail page (`POST /appointments/{id}/mercado-pago/{payment}/refund`); `/settings/mercado-pago` is read-only.
- Amounts are sent as JSON numbers from centavos using the shortest round-trip form of the double, which is the exact two-decimal value for prices below 10^13 centavos (unit test round-trips every value up to $10,000.00 plus large samples). Received amounts are parsed back to centavos exactly; more than two decimals is a mismatch.
- Synthetic evidence: `tests.rs` (config, idempotency keys, amount round trip, exact request shapes against a local Axum provider, untrusted checkout hosts and error statuses) and `worker_tests.rs` (disposable Postgres: booking → preference → WhatsApp link, outage fallback, reschedule, matching vs mismatched payments, idempotent polling, external chargeback, late/duplicate/cancelled refunds on Doctor request, rejected refund, deadline close + late checks, absent configuration, rejected token, startup error without echo).
- Still open (operator, not claimed by tests): sandbox run with a test seller/buyer, confirming the MX excluded payment-type IDs and `binary_mode`, real refund, rate limits for the 30-second poll, and live acceptance.
