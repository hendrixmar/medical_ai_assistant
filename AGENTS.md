# Medical Assistant — agent contract

## Safety boundary

- This is a secure dashboard foundation for one Practice and one Doctor per deployment, not a clinical record or ready product release. Do not add longitudinal patient records or imply unavailable Appointment journeys are implemented.
- A Patient is named on one Appointment only; a Receipt is transfer evidence, never proof funds arrived. Only the Doctor's explicit Payment Verification confirms receipt.
- Never log, print, commit, or send passwords, TOTP/encryption keys, enrollment URIs, cookies, tokens, OAuth credentials, or patient/contact data. Doctor enrollment and deployment remain operator-only.

## Project workflow

Before Rust/API/domain, auth, integration, UI, test, or release-gate work, read `.agents/skills/medical-assistant-workflow/SKILL.md` with `CONTEXT.md`, relevant ADRs, and the matching README/integration evidence.

## Agent skills

### Issue tracker

Issues and specs live in GitHub Issues for `hendrixmar/medical_ai_assistant`.
See `docs/agents/issue-tracker.md` before tracker operations.

### Triage labels

Use the five default canonical triage labels.
See `docs/agents/triage-labels.md` before applying labels.

### Domain docs

Single-context layout: root `CONTEXT.md` and `docs/adr/`.
See `docs/agents/domain.md` before exploring the codebase.
