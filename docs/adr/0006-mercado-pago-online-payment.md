# Use Mercado Pago Checkout Pro with the Doctor's own access token for optional video payment

Status: **accepted** (Hendrik, 2026-11-04: D1–D5 as recommended). Supersedes [ADR 0004](0004-stripe-connect-online-payment.md).

Hendrik asked to replace Stripe with Mercado Pago as the online payment option for video Appointments. Bank transfer with Receipt review and the Doctor's explicit Payment Verification stays unchanged and remains the fallback. The full integration contract, test plan and open decisions are in [docs/integrations/mercado-pago.md](../integrations/mercado-pago.md).

## Use the Doctor's own access token, not Mercado Pago OAuth

Under [ADR 0001](0001-one-deployment-per-practice.md) a deployment serves one Practice. The Doctor creates an application in their own Mercado Pago account and the operator stores its production access token as a server-only secret. Payments land directly in the Doctor's account; Mercado Pago fees are charged to it; the platform takes no fee.

Mercado Pago OAuth (the marketplace model) is rejected for v1. It exists to act for many sellers under one platform application and to charge a `marketplace_fee`. Here there is one seller and no fee. OAuth would add a platform application and client secret owned by Hendrik, an authorization callback, refresh-token rotation and a re-authorization path for 180-day tokens, all with no benefit for a single-seller deployment.

## Create one Checkout Pro preference per payment window

After the booking transaction commits, a durable `appointment_jobs` job creates a Checkout Pro preference for the Appointment's revision. Provider calls stay outside the booking transaction; a Mercado Pago outage does not block booking or bank-transfer instructions. The preference carries one item in `MXN` at the Appointment's snapshotted price, an opaque `external_reference` derived from the Appointment ID and revision, and `expires=true` with `expiration_date_to` equal to the Receipt Deadline, so the preference has a native expiry. Titles and references contain no patient or Contact data. The existing outbox sends the `init_point` URL with the WhatsApp payment instructions.

Only immediate methods are accepted (cards and Mercado Pago account money). Cash (`ticket`, e.g. OXXO), `atm` and bank-transfer types are excluded because Mercado Pago recommends at least three days for them while the Receipt window is configured in minutes (120 by default). This keeps the same reasoning that made ADR 0004 card-only.

## Confirm only from Mercado Pago's API, by polling first

A Payment Link is never proof. The System marks a video Appointment paid only after it reads the payment from Mercado Pago's API (`GET /v1/payments/search?external_reference=…` or `GET /v1/payments/{id}`) and finds `status=approved`, `currency_id=MXN`, `transaction_amount` equal to the stored price, the expected `external_reference`, and `collector_id` equal to the Doctor's Mercado Pago user. Appointment History records actor `System`. Every Mercado Pago payment ID is recorded once, which makes confirmation idempotent and lets the app detect duplicate and late payments.

The app's origin is tailnet-private, so Mercado Pago cannot deliver webhooks to it today. A server worker polls Mercado Pago, as the Stripe worker did. A signed webhook (`x-signature`, HMAC-SHA256) is specified as an optional accelerator. It only schedules an immediate API read and never confirms from the notification body. It requires a public route, which is an exposure change that needs Hendrik's explicit approval.

## Refund only on the Doctor's explicit action

Cancellation after a Mercado Pago-confirmed payment keeps the existing `refund_due` Attention Item. The Doctor may press **Reembolsar con Mercado Pago**, which runs a durable refund job with a deterministic `X-Idempotency-Key`. The app never refunds automatically. Refund or chargeback never reverses Payment Verification; it records history and attention. Late and duplicate payments get the same Doctor-triggered refund option.

## Consequences

The Doctor needs a Mercado Pago account with an application and production credentials; the operator handles the token like any other provider secret. Stripe Connect code, its uncommitted migration, its environment variable and its documentation are removed. No committed revision ever contained Stripe, so no deployed database can hold Stripe data. Only cards and account money are offered online; OXXO and SPEI users still pay by bank transfer. Without the optional webhook, chargebacks are detected only by periodic rechecks. Live eligibility, real payment, refund behavior and webhook signing must be verified separately by an operator in Mercado Pago's test environment; synthetic tests cannot establish them.
