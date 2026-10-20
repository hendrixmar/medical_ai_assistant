---
name: medical-assistant-workflow
description: Change and verify the Medical Assistant Rust service, Doctor access, appointments, payments, WhatsApp, or Google integrations while preserving patient and credential safety.
---

# Medical Assistant workflow

Use for Rust/API/domain, authentication, integration, UI, test, or release-gate work.

## Read the authoritative sources

- Read root `AGENTS.md`, `CONTEXT.md`, relevant `docs/adr/`, and the matching README section before changing a contract. Use `docs/agents/domain.md` for vocabulary/ADR rules; read `docs/agents/issue-tracker.md` or `triage-labels.md` before tracker work.
- For integration changes, read the specific evidence/operations record first: `docs/integrations/waha.md`, `waha-messaging.md`, `doctor-number.md`, `conversation-outbox.md`, or `google-calendar.md`. Distinguish synthetic/local evidence from live-provider acceptance.
- Inspect `src/modules/`, `src/database/`, migrations, and the relevant tests. Preserve existing Rust/Cargo.lock; run project commands from repository root.

## Stack and verification

- Rust 2021, Axum, SeaORM/Postgres, SeaORM migrations, Cargo.lock. README requires Rust 1.88+; Docker Compose v2 is only needed for the isolated Postgres acceptance fixture.
- Fast checks: `cargo fmt --check`; `cargo clippy --locked --all-targets -- -D warnings`; `cargo build --locked --bins`; `cargo test --locked`.
- `cargo test --locked` excludes ignored database tests and is not full acceptance. Full database acceptance must use disposable Postgres with explicit `TEST_DATABASE_URL` to a database named `medical_assistant_test` (or another `_test` database), never `.env` or the legacy `example` DB. Supply the test password privately and without shell tracing, as in README.
- From repository root, start and clean only the isolated fixture: `docker compose --env-file /dev/null -p medical-assistant-tests -f compose.test.yml up -d --wait`, then `cargo test --locked -- --include-ignored`, then `docker compose --env-file /dev/null -p medical-assistant-tests -f compose.test.yml down`. Use an unused loopback test port and the same unique Compose project throughout; allow the long synthetic-send test over two minutes.
- Local server migration/run commands apply only to a newly provisioned development database. Read the README first; Doctor enrollment, private-HTTPS preview, and real-provider acceptance are human/operator steps.

## Patient, payment, and credential boundaries

- This is a secure dashboard foundation for one Practice and one Doctor per deployment, not a clinical record or ready product release. Do not create longitudinal patient records or imply appointment management is available.
- A Patient is named on one Appointment only; a Contact may book for several Patients. Keep `CONTEXT.md` vocabulary and the single-Practice ADR.
- A Receipt is evidence of a transfer, never proof funds arrived. Only the Doctor's explicit Payment Verification marks a video appointment paid; CLABE is 18 ASCII digits stored as text, format-only.
- Never log, print, commit, or send passwords, TOTP/encryption keys, enrollment URIs, cookies, tokens, OAuth credentials, receipt content, or patient/contact data. `bootstrap-doctor` enrollment and deployment remain operator-only; do not automate them or use a real provider during tests.
