# Use Stripe Connect Payment Links for optional video card payment

A video Appointment can be paid by bank transfer or card. The Doctor explicitly verifies a bank transfer. The System confirms a matching Stripe card payment. The Practice uses one Stripe platform account. Each Doctor connects their own Stripe account through Stripe-hosted onboarding. The Doctor never receives or enters the platform API key.

The connected account gives the Doctor full Stripe Dashboard access. Stripe collects account requirements. Stripe fees go to the connected account, and Stripe takes payment losses. The platform takes no application fee. Direct charges credit the Doctor's account. Bank transfer remains available as a fallback.

## Create a Payment Link after booking

A durable job creates a Price and Payment Link after the booking transaction commits. Both calls use idempotency keys derived from the Appointment, revision, and purpose. Provider requests stay outside the booking transaction. A Stripe outage does not block booking or bank-transfer instructions. Product names and metadata contain no patient or Contact data.

Use Payment Links instead of creating Checkout Sessions directly. A Contact may open the link on a phone that cannot reach the private tailnet origin. The link uses a hosted confirmation message instead of a `success_url`. The app polls the Checkout Sessions that Stripe creates for the link. It confirms payment only when Stripe reports `status=complete`, `payment_status=paid`, `currency=mxn`, and an `amount_total` that matches the Appointment's snapshotted price. The System records the confirmation.

The link accepts cards only and allows one completed session. Asynchronous methods such as OXXO can remain pending for days, beyond the Appointment's Hold. Card-only payment avoids that mismatch. Rescheduling keeps the same Payment Link and Receipt Deadline.

## Poll instead of using webhooks

Stripe cannot reach the application's private tailnet origin. The platform account also has a shared limit of 16 Connect webhook endpoints across deployments. A worker polls Stripe from the server. The integration registers no webhook.

A Payment Link has no native expiry. The app deactivates it at the Receipt Deadline, on cancellation or expiry, after payment verification, or when Doctor rejection opens a new Receipt window. A payment that arrives after the Appointment can no longer accept it is recorded as late or duplicate attention. The app does not confirm it as a new payment.

## Consequences

The Doctor can receive payment without manual transfer review. The server must first match Stripe's session status, currency, and amount to the stored Appointment. Card payments can still be late or duplicated. The app makes no refund API call. The Doctor handles any refund in Stripe and records the existing refund-resolution action in the dashboard. That action does not transfer funds or reverse verified Payment. Live Stripe eligibility, restricted-key permissions, and real payment acceptance require separate operator verification. Synthetic tests cannot establish them.
