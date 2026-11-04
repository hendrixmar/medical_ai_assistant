# Stripe Connect card payments

This record describes the integration and operator requirements. It does not record acceptance of a Stripe platform account, restricted key, connected account, or live card payment.

## Payment and account model

The deployment uses one Stripe platform account. A Doctor connects their own Stripe account from **Configuración → Conexiones** and completes Stripe-hosted onboarding. The Doctor never enters an API key.

The app creates an `MX` connected account. It requests a full Stripe Dashboard and Stripe-hosted requirement collection. Stripe fees go to the connected account, and Stripe takes payment losses. The platform takes no application fee.

A connection is ready for card-payment links when Stripe reports `charges_enabled=true`. Direct charges also require an active `card_payments` capability. Confirm both before live use. See Stripe's [Connect charge guide](https://docs.stripe.com/connect/charges).

For each eligible video Appointment, the app creates a Price and Payment Link on the Doctor's connected account. Each request uses `Stripe-Account: acct_…`. Direct charges appear in the connected account's balance. The app accepts cards only. Bank transfer and Receipt review remain available.

A Payment Link is not confirmation. A worker lists the Checkout Sessions for each link. It accepts a session only when `payment_status=paid`, `status=complete`, `currency=mxn`, and `amount_total` matches the Appointment's stored price.
Appointment History records actor `System` with the confirmation. The app also stores the paid session ID.
A hosted message tells the Contact that payment was received and confirmation will arrive by WhatsApp. A browser return does not confirm payment.

Stripe Payment Links do not expire on their own. The worker deactivates a link at the Receipt Deadline, after cancellation or expiry, after payment verification, or when Doctor rejection opens a new Receipt window. Rescheduling keeps the same link and deadline.

Disconnecting Stripe forgets the account ID locally after a best-effort attempt to deactivate open links. It does not delete the Doctor's Stripe account. New bookings then use bank transfer only.

Stripe metadata contains Appointment ID, revision, and Payment Reference. Product names contain the consultation type and local start time. Do not add patient or Contact names, phone numbers, or other personal details.

A Receipt is transfer evidence only. It never proves funds arrived. The Doctor's explicit Payment Verification can confirm a bank transfer. Only a matching, server-confirmed Stripe card payment can confirm a card payment.

## Why there are no webhooks

Stripe cannot reach the app's private tailnet origin. The platform account also shares a limit of 16 Connect webhook endpoints across deployments. A worker polls Stripe from the server. The integration registers no webhook, so this flow needs no webhook signing secret or public Stripe callback route.

## Operator requirements

Register the platform account for Connect and confirm Connect is enabled. Before live use, verify that Stripe currently permits the account to create connected accounts in Mexico. The app and its synthetic fixture do not establish platform eligibility. Stripe's [Account creation reference](https://docs.stripe.com/api/accounts/create) says that a platform must register before it creates connected accounts.

Set `STRIPE_SECRET_KEY` only in the server's private secret store or the deployment's app-only provider environment file. The setting is optional. When absent, Stripe is disabled. A supplied key must start with `sk_` or `rk_`, contain printable characters only, contain no whitespace, and be at most 255 characters. Never put it in source, a committed environment file, command arguments, logs, screenshots, or Doctor-facing settings. Stripe keys are server-only secrets. Stripe recommends restricted API keys (`rk_`) because you can limit their permissions.

Before removing `STRIPE_SECRET_KEY`, use **Desconectar Stripe** while the key is still available. Otherwise the app cannot poll or deactivate existing Payment Links; Stripe may still accept a payment the app cannot confirm.

## Restricted-key permissions

The table lists the operations the app needs. These are operator requirements derived from the API calls. They do not describe a permission set verified in the current Stripe account.

| Stripe resource | Required operations | Restricted-key level to configure |
| --- | --- | --- |
| Accounts | `POST /v1/accounts`; `GET /v1/accounts/{id}` | Write. Write also permits Read. |
| Account Links | `POST /v1/account_links` | Write. |
| Prices | `POST /v1/prices` | Write. |
| Payment Links | `POST /v1/payment_links`; `POST /v1/payment_links/{id}` to set `active=false` | Write. |
| Checkout Sessions | `GET /v1/checkout/sessions` filtered by `payment_link` | Read. |
| Connected-account access | The account-scoped calls above send `Stripe-Account` | Enable the restricted key's Connect access for connected accounts. |
| Products | `product_data[name]` on `POST /v1/prices` creates a Product | Confirm whether the Dashboard requires Products: Write in a sandbox. This mapping is not verified here. |

Stripe's [restricted API key guide](https://docs.stripe.com/keys/restricted-api-keys) defines resource-level `Read`, `Write`, and `None` permissions. It maps `GET` to Read and `POST` or `DELETE` to Write. Write includes Read. The Price request uses `product_data[name]` to create a Product. Confirm whether that nested operation also requires Products: Write with a sandbox restricted key and its request logs. Confirm the exact resource settings before creating a live key.

The app does not call the Refunds API, create Checkout Sessions directly, or register webhooks. Do not grant those permissions for this flow unless the implementation changes. See Stripe's API references for [Account creation](https://docs.stripe.com/api/accounts/create), [Account Links](https://docs.stripe.com/api/account_links/create), [Price creation](https://docs.stripe.com/api/prices/create), [Payment Link creation](https://docs.stripe.com/api/payment-link/create), [Payment Link updates](https://docs.stripe.com/api/payment-link/update), and [Checkout Session listing](https://docs.stripe.com/api/checkout/sessions/list).

## Request and data safety

The production client uses the fixed base URL `https://api.stripe.com/v1/`. Only test builds can override it. The client disables proxies, redirects, and retries. It uses a two-second connection timeout and a five-second request timeout. It sends the key in a sensitive Bearer authorization header. Keep the key and full Payment Link URLs out of logs. Payment metadata contains no patient or Contact details.

## Synthetic verification and live acceptance

The client tests in `src/modules/stripe/tests.rs` use a local Axum HTTP fixture. They cover optional and malformed key configuration, static errors that do not echo key values, and distinct idempotency keys. They assert the exact Account, Account Link, Price, Payment Link, Checkout Session-list, and Payment Link-deactivation requests.

The tests check the connected-account header, card-only link parameters, amount and currency, Appointment metadata, and that the Authorization header is marked sensitive. They exercise local request construction only. They do not call Stripe. This record does not claim that the tests passed.

Separate database tests in `src/modules/stripe/worker_tests.rs` use the same local Stripe fixture plus the disposable Postgres fixture. They exercise protected onboarding, durable link-job and outbox behavior, matching-session confirmation, amount/currency rejection, late and duplicate attention, and deadline deactivation. They use synthetic provider responses, do not call live Stripe, and this record does not claim that the tests passed.

The client fixture does not prove that Stripe accepts a request, that the platform can onboard an MX account, that a restricted key has the needed permissions, that a real card payment settles, or that a Doctor's deployment can reach live Stripe. The fixture also does not establish booking and outbox fallback, worker confirmation, late or duplicate payment handling, or database-backed deadline deactivation.

No live Stripe setup or acceptance is recorded here. An operator must separately verify platform Connect enablement, current Mexico eligibility, sandbox restricted-key permissions, connected-account onboarding and capability state, and live card acceptance before enabling production payments. No production key or account credentials belong in the repository.

## Refund behavior

The application never issues or verifies a Stripe refund. A cancellation after verified payment keeps the existing `refund_due` Attention Item. The Doctor handles any card refund in Stripe's Dashboard and then records the existing **Marcar reembolso como resuelto** action. That action resolves attention and records history. It does not move funds or reverse Payment Verification.

Late or duplicate Stripe payments require the Doctor to refund one payment in Stripe or contact the Contact. The application does not refund them automatically.
