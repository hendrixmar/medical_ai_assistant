# Brief traceability matrix

Provenance: "Más Consultas — Brief tecnológico y propuesta comercial con Hendrik
Martina", *Versión final*, 19 de octubre de 2026 (391 lines), re-read at
execution time on 2026-10-20 from the ThinkPad source copy. Dispositions follow
rev.2 authority only: `AGENTS.md` safety boundary, `CONTEXT.md`, ADR-0001
(one Practice/Doctor per deployment), ADR-0002 (LLM only parses; the
application states facts), ADR-0003 (Google Calendar REST, not MCP), and the
approved issue #2 scope. `Rejected by rev.2` means excluded from the approved
current implementation scope, not that the commercial proposal was permanently
rejected. `Implemented` rows were spot-checked against the route/module
inventory merged in `src/modules/doctor/http.rs`, not copied from brief text.

Disposition vocabulary (exactly one per functional row): `Implemented`,
`Phase 2 per brief §16`, `Rejected by rev.2`, `Future issue filed`,
`ADR decision required`, `Operator/legal gate`.

## Invariants (must read Implemented)

| Brief ref | Requirement (paraphrase) | Disposition | Authority / where |
|---|---|---|---|
| §10 restricción crítica | A transfer image never by itself proves funds arrived; no automatic payment confirmation from a capture | Implemented | `Payment Verification` is Doctor-only: `src/modules/payments/actions.rs` `verify`; `CONTEXT.md` `Receipt`, `Payment Verification` |
| §5.1 / §15.2 | No double-booking of the same time | Implemented | Hold exclusion constraint `appointment_holds_no_cross_appointment_overlap` (EXCLUDE USING gist) in `src/database/migrations/appointments.sql`; `src/modules/appointments/booking.rs` |
| §1 / §8 fuera-del-MVP | No clinical record in the MVP | Implemented | `CONTEXT.md` `Patient` (name on one Appointment only); `AGENTS.md` safety boundary; no clinical tables/routes in the module inventory |
| §4 regla de conocimiento | The agent never invents availability, prices, credentials or policies; unsupported answers defer to a person | Implemented | ADR-0002; closed parser `src/modules/conversation/parser.rs`, application-rendered `src/modules/conversation/templates.rs`, Followup Request path |

## §2 Experience and safety principles

| Brief ref | Requirement (paraphrase) | Disposition | Authority / where |
|---|---|---|---|
| §2 | Clear, simple, human conversation in Spanish | Implemented | Deterministic Spanish templates in `src/modules/conversation/templates.rs`; menu in conversation flow |
| §2 | Orientation toward booking without commercial pressure | Implemented | Deterministic booking flow `src/modules/appointments/conversation.rs`; no sales-pressure copy in templates |
| §2 | Identify as virtual assistant; never impersonate the Doctor/staff | Implemented | Privacy-prefixed first response and assistant copy in `src/modules/conversation/templates.rs` / views |
| §2 | No diagnosis, treatment or health promises; escalate clinical requests to authorized professionals | Implemented | Closed intent set (ADR-0002); anything outside booking rules becomes a Followup Request (`CONTEXT.md` `Followup Request`) |
| §2 | Human control: Doctor/reception can review, confirm, correct or stop the agent | Implemented | Doctor-only dashboard decisions (payments, Followups, Takeover in `src/modules/whatsapp/takeover.rs`); outbound/Takeover default OFF. Reception as a separate role: see §12 row |
| §2 | Privacy by design: request only necessary data; separate data between practices | Implemented | One deployment per Practice (ADR-0001) — no cross-practice data exists in a deployment; data minimization in conversation flow |
| §2 | Configurability per Doctor: agenda rules, prices, policies, reminders, authorizations | Implemented | Practice four-step settings in `src/modules/practice/` (details, services, schedule, payments) |

## §3 WhatsApp agent capabilities

| Brief ref | Requirement (paraphrase) | Disposition | Authority / where |
|---|---|---|---|
| §3.1 | Official WhatsApp Business Platform integration | Rejected by rev.2 | Issue #2 Solution + `docs/integrations/waha.md`: WAHA/GOWS on the Practice Number instead of the official Platform |
| §3.1 | Receive automatic messages from campaigns and direct Contacts | Implemented | Signed WAHA webhook intake `POST /api/v1/webhooks/waha` in `src/modules/whatsapp/http.rs`; `src/modules/whatsapp/inbox.rs` |
| §3.1 | Understand free text / open questions; context memory within the authorized conversation | Implemented | ADR-0002 closed parser (`src/modules/conversation/parser.rs`, optional Claude preference parsing) + durable conversation state `src/modules/conversation/` |
| §3.1 | Specific answers on specialty, track record, consultation types, price, location, duration, administrative preparation | Future issue filed | #30. Doctor-authored knowledge answers are a future delta; today only Practice/Service facts configured in `src/modules/practice/` are rendered |
| §3.1 | Everyday language and emojis when appropriate; per-practice instruction personalization | Rejected by rev.2 | ADR-0002: factual copy is application-rendered templates, not per-practice generated tone; `src/modules/conversation/templates.rs` |
| §3.1 | Recover interrupted conversations within consent/messaging rules | Implemented | Durable inbox + conversation jobs and WAHA recovery/backfill in `src/modules/whatsapp/recovery.rs`; restart-safe processing jobs |
| §3.1 | Transfer to reception on request, unresolved doubt or sensitive case | Implemented | Followup Requests + Contact-scoped Takeover (`src/modules/whatsapp/takeover.rs`); sensitive/out-of-scope input becomes a Followup Request |
| §3.2 | Identify administrative reason and consultation of interest without requesting unnecessary clinical details | Implemented | Closed intents in `src/modules/conversation/parser.rs`; booking collects Patient name/Service/Format only |
| §3.2 | Resolve price/schedule/location/process objections using authorized information | Implemented | Only configured Practice/Service facts are rendered (ADR-0002); unknown matters become Followup Requests. Extended FAQ prose: future issue (below) |
| §3.2 | Offer real schedules and lead to a concrete booking request/reservation | Implemented | Availability from Practice rules + Holds + Google Blocking Calendars; `src/modules/appointments/availability.rs`, `booking.rs` |
| §3.2 | Register prospect/campaign origin when technically valid traceability exists | Rejected by rev.2 | Issue #2 Out-of-Scope (analytics/attribution); no campaign source is recorded on Appointments |
| §3.2 | Consented follow-up of interested Contacts who did not finish booking, without insistent/deceptive messages | Rejected by rev.2 | Not in approved scope; only Appointment Reminders to confirmed Appointments are sent (`CONTEXT.md` `Reminder`). No marketing follow-up automation |
| §3.2 | Identify possible urgencies and immediately refer to human medical care/emergency services, without autonomous clinical triage | Implemented | Out-of-scope/clinical content is not handled by the closed parser and becomes a Followup Request for the Doctor; no triage is performed (ADR-0002) |
| §3.3 | Voice-note transcription (ampliable) | Phase 2 per brief §16 | Brief §3.3 itself defers; also issue #2 Out-of-Scope (speech-to-text) |
| §3.3 | Image analysis for administrative documents (ampliable) | Phase 2 per brief §16 | Brief §3.3 defers; Receipt media is copied for Doctor review, never OCR-analysed (see §10.1) |
| §3.3 | Multichannel consultation and staff suggestions (ampliable) | Phase 2 per brief §16 | Brief §3.3 defers; single WhatsApp channel only (ADR-0001) |

## §4 Knowledge base and practice identity

| Brief ref | Requirement (paraphrase) | Disposition | Authority / where |
|---|---|---|---|
| §4 | Each Doctor administers content, approves answers and updates agent knowledge without code changes | Future issue filed | #30. Configuration of Practice/Services exists (`src/modules/practice/`); Doctor-approved knowledge answers are the #30 future issue |
| §4 | Professional data: full name, specialty, licences (cédulas) and training, validated before publication | Future issue filed | Same knowledge/profile future issue; no professional-profile surface today |
| §4 | Services: consultations, announcable procedures, in-person/online format, prices, location, attention times | Implemented | Services + Practice settings in `src/modules/practice/` (stable IDs, MXN centavos, Format in `CONTEXT.md`) |
| §4 | Operation: schedules, appointment duration, advance-payment/rebooking/cancellation policies, non-working days | Implemented | Schedule step in `src/modules/practice/` (working hours, buffers, horizon, cancellation policy text); holidays only as closed days in weekly rules |
| §4 | Communication: tone of voice, greetings, frequent answers, allowed vocabulary, emoji use | Future issue filed | Knowledge-answers future issue covers Doctor-approved verbatim answers only; tone generation stays `Rejected by rev.2` per ADR-0002 |
| §4 | Authorized material: PDFs, text documents, approved answers, institutional administrative guides | Future issue filed | Future issue scope is verbatim stored answers only; no document/RAG ingestion (ADR-0002) |
| §4 | Versioning and approval: date, user who changed information, publication state | Future issue filed | To be decided in the knowledge future issue; Practice settings today are Doctor-saved configuration without a publication workflow |
| §4 regla | If an answer is not backed by approved information, recognize the limitation and refer to a person; never invent | Implemented | See invariant row; Followup Request path in conversation flow |

## §5 Administrative CRM and digital agenda

| Brief ref | Requirement (paraphrase) | Disposition | Authority / where |
|---|---|---|---|
| §5.1 | Daily, weekly and monthly calendar views optimized for computer and phone | Future issue filed | #28. Dashboard shows attention + upcoming Appointments list, not calendar grid views |
| §5.1 | Manual creation, editing, cancellation and reprogramming by Doctor/reception | Future issue filed | #28. Manual dashboard creation is issue #2 Out-of-Scope today; Doctor dashboard cancellation/rescheduling of existing Appointments is Implemented in `src/modules/appointments/http.rs`. Creation is #28 |
| §5.1 | AI-originated appointments immediately visible in the same calendar | Implemented | Same Appointment store feeds dashboard list/detail (`src/modules/appointments/http.rs`) and Google Booking Calendar sync |
| §5.1 | Working hours, duration per appointment type, exceptions, vacations, blocks; multiple professionals/sites when scope expands | Implemented | Working-hours/schedule configuration in `src/modules/practice/` + Google Blocking Calendars make time unavailable. Multiple professionals/sites: `Rejected by rev.2` (ADR-0001, one Practice/Doctor) |
| §5.1 | Transactional control to avoid double booking; temporary retention while confirming | Implemented | Invariant row: Hold exclusion constraint + Hold expiry (`CONTEXT.md` `Hold`) |
| §5.1 | Appointment source registration (ad, organic WhatsApp, reception, Doctor, referral, other) | Rejected by rev.2 | Issue #2 Out-of-Scope (analytics); Appointment records carry actor history, not marketing source |
| §5.2 | Appointment states Solicitada / Apartada / Pendiente de aprobación / Pendiente de pago / Confirmada per brief definitions | Implemented | Mapped to Appointment status + Hold + payment state in `src/modules/appointments/` and `CONTEXT.md` `Appointment`; internal approval is the Doctor's Payment Verification for video |
| §5.2 | Appointment states Realizada / No asistió | ADR decision required | Reverses `CONTEXT.md` `Appointment` ("there is no completed or no-show"); Step-5 ADR decision issue only (number below) |
| §5.2 | Reprogramada / Cancelada recorded with reason, actor and moment | Implemented | Append-only Appointment History with actor (`CONTEXT.md` `Appointment History`), `src/modules/appointments/booking.rs` reschedule keeps the same Appointment |
| §5.2 | Practice chooses automatic confirmation or mandatory internal confirmation; agent never announces confirmed before conditions are met | Implemented | Video Appointments require Receipt + Doctor Payment Verification before confirmed; in-person flow per Practice rules in `src/modules/appointments/booking.rs` |

## §6 Bidirectional agent–agenda integration

| Brief ref | Requirement (paraphrase) | Disposition | Authority / where |
|---|---|---|---|
| §6 | Agent accesses agenda through internal APIs and controlled functions, never free/unlimited database access | Implemented | ADR-0002 + ADR-0003: application booking rules own all calendar operations; the LLM only parses |
| §6 | Consult real availability and alternative schedules | Implemented | `src/modules/appointments/availability.rs` |
| §6 | Temporarily hold a schedule and release the retention on expiry | Implemented | Holds + expiry timers in `src/modules/appointments/timers.rs`; `CONTEXT.md` `Hold`, `Expired Appointment` |
| §6 | Create requests and consult their state | Implemented | Booking conversation + Appointment detail/history for the Doctor; Contact obtains state through conversation templates |
| §6 | Modify, cancel or reschedule per permissions and identity checks | Implemented | Contact reschedule/cancel in conversation flow; Doctor cancel/reschedule in dashboard; Doctor Number is read-only and cannot change records (`CONTEXT.md`) |
| §6 | Immediately reflect manual changes by reception/Doctor | Implemented | Single Appointment store; dashboard changes write the same Holds/outbox fences the conversation flow reads |
| §6 | Manage retries and failures without duplicate appointments or contradictory confirmations | Implemented | Idempotency fences, revision checks and one durable outbox in `src/modules/appointments/booking.rs`; uncertain sends are never automatically resent |
| §6 flujo | Campaign/contact → WhatsApp → doubts → availability → hold → internal authorization → verified payment → confirmed → reminders → realized → report | Implemented | Journey implemented through confirmation + reminders. "Realizada/reporte" tail: `ADR decision required` / `Rejected by rev.2` (no attendance states or reporting; see §5.2/§13) |

## §7 Internal WhatsApp for Doctor and reception

| Brief ref | Requirement (paraphrase) | Disposition | Authority / where |
|---|---|---|---|
| §7 | Notify new requests to an authorized number/group without unnecessary clinical data | Implemented | Doctor notifications via the verified Doctor Number (`src/modules/doctor_number/`, `src/modules/conversation/` Doctor notification outbox) |
| §7 | Approve, reject or request modification from a verified WhatsApp flow | Rejected by rev.2 | `CONTEXT.md` `Doctor Number`: it cannot change records; decisions happen in the authenticated dashboard only |
| §7 | Consult today's appointments, free spaces and pending requests per role | Implemented | Doctor Number read-only agenda answers (`src/modules/conversation/agenda.rs`); no roles (one Doctor) |
| §7 | Confirm changes in the CRM and send the patient the final result, avoiding premature replies | Implemented | Application-rendered confirmations only after committed state (ADR-0002, outbox fences in `src/modules/appointments/booking.rs`) |
| §7 | Escalate sensitive operations to a secure interface requiring additional authentication | Implemented | Sensitive operations require dashboard password+TOTP session (`src/modules/doctor/http.rs` authorize) |
| §7 | Phone-number recognition alone never authorizes sensitive changes; propose authentication, role controls and audit records | Implemented | Doctor Number binding proof is ownership-only and read-only; dashboard auth + append-only Appointment History provide audit |

## §8 Patient administration — initial scope

| Brief ref | Requirement (paraphrase) | Disposition | Authority / where |
|---|---|---|---|
| §8 | Administrative patient fichas (not an electronic clinical record); UI must not imply a clinical system | ADR decision required | Reverses `AGENTS.md` safety boundary + `CONTEXT.md` `Patient`; Step-5 ADR decision issue only |
| §8 | Name, phone, email when applicable, and minimum booking data | ADR decision required | Today a Patient is only a name on one Appointment (`CONTEXT.md`); Contact identity is the WhatsApp number. Persistent fichas need the ADR decision |
| §8 | Administrative history of appointments, cancellations, confirmations and payments | ADR decision required | Per-Appointment History exists; a longitudinal per-Patient history requires the ficha ADR decision |
| §8 | Communication preferences and consent where applicable | Operator/legal gate | Consent model/privacy notice remain provisional release gates (README operator runbook, §14 rows) |
| §8 | Exclusively administrative notes, avoiding diagnoses/clinical antecedents | ADR decision required | Notes on a persistent ficha are part of the ADR decision issue |
| §8 | Administrative files needed for the appointment under access/retention controls | Implemented | Receipt files are Appointment-linked, access-controlled and retention-managed (`src/modules/payments/`, retention in `src/modules/appointments/retention.rs`) |
| §8 fuera-del-MVP | No clinical history, evolution notes, diagnoses, prescriptions, lab results, imaging or full EHR | Implemented | Invariant row: no clinical record in the MVP |

## §9 Reminders, confirmations and rescheduling

| Brief ref | Requirement (paraphrase) | Disposition | Authority / where |
|---|---|---|---|
| §9 | On confirmation: send appointment data, location and administrative instructions | Implemented | Application-rendered confirmation via the shared outbox (`src/modules/appointments/`) |
| §9 | 72 hours before: first notice with option to confirm or request change | Future issue filed | #29. No 72h Reminder exists today |
| §9 | 24 hours before: second reminder per Doctor preferences/policy | Implemented | 24h default Reminder in `src/modules/appointments/timers.rs` |
| §9 | (Brief extension in current code) optional 2h reminder | Implemented | `reminder_2h_enabled=false` default, `src/modules/appointments/timers.rs`; configurable sequence is the future issue |
| §9 | On reply: update appointment state and notify reception when needed | Implemented | Attendance replies are handled separately from booking replies in `src/modules/appointments/timers.rs` / conversation flow |
| §9 | On cancellation: release availability and apply defined policy | Implemented | Cancellation frees the Hold and renders the configured cancellation policy (`src/modules/appointments/booking.rs`) |
| §9 | On silence: escalate when the practice requests human intervention | Implemented | Unanswered/uncertain work surfaces as Attention Items for the Doctor (`CONTEXT.md` `Attention Item`) |
| §9 | Sequence and frequency personalized per practice; Meta consent/template/window rules | Future issue filed | Sequence configurability is the Step-5 future issue. Meta template/window regime: `Rejected by rev.2` for this deployment (WAHA/GOWS channel, `docs/integrations/waha.md`), with outbound-sending consent as `Operator/legal gate` |

## §10 Payments and receipts

| Brief ref | Requirement (paraphrase) | Disposition | Authority / where |
|---|---|---|---|
| §10.1 | Show authorized bank data and the advance amount | Implemented | Bank/CLABE configuration (format-only, 18 digits as text) rendered from Practice settings; `CONTEXT.md` `Payment Reference` |
| §10.1 | Receive transfer images/PDF and relate them to the appointment record | Implemented | Receipt intake/copy and Appointment linking in `src/modules/payments/intake.rs` |
| §10.1 | Extract amount, reference, date and bank by OCR/AI | Phase 2 per brief §16 | Brief §10.1 note defers extraction; issue #2 Out-of-Scope (OCR extraction). Doctor reviews the copied Receipt directly |
| §10.1 | Keep pending state until verifiable bank reconciliation or authorized human approval | Implemented | Only the Doctor's explicit Payment Verification confirms (invariant row) |
| §10.2 | Online payment gateway connected to the Doctor's own account | Phase 2 per brief §16 | Brief §10.2 is a later phase; issue #2 Out-of-Scope (payment processors) |
| §10.2 | Secure payment link from WhatsApp | Phase 2 per brief §16 | Same Phase-2 online-payment delta |
| §10.2 | Validate signed gateway events and consult real transaction state | Phase 2 per brief §16 | Same Phase-2 online-payment delta |
| §10.2 | Confirm appointment only if payment was approved and other requirements met | Phase 2 per brief §16 | Same delta; today's confirmation gate is Doctor Payment Verification for transfers |
| §10.2 | Record failed payments, refunds and state changes without double charges | Implemented | Manual-refund resolution is Doctor-driven and idempotent in `src/modules/payments/actions.rs` (transfer flow). Gateway refunds: `Phase 2 per brief §16` |

## §11 Personalization and institutional information

| Brief ref | Requirement (paraphrase) | Disposition | Authority / where |
|---|---|---|---|
| §11 | Configurable white-label: logo, colours, image and professional name per Doctor | Rejected by rev.2 | `PRODUCT.md` Voice and Identity: no established visual identity/invented brand; Practice name is configuration, branding is not |
| §11 | Visible profile: specialty, location, phone, verified professional data | Future issue filed | Professional-profile facts belong to the knowledge/profile future issue; Practice location/phone configuration exists in `src/modules/practice/` |
| §11 | Private institutional repository for titles, licences, COFEPRIS notices and advertising documentation | Rejected by rev.2 | No document repository in approved scope; no ingestion (ADR-0002, issue #2 Out-of-Scope document/RAG) |
| §11 | Metadata and review/expiry reminders for institutional documents where applicable | Rejected by rev.2 | Same repository exclusion above |
| §11 | Total separation between institutional documents and patient fichas | Implemented | No institutional repository and no patient fichas exist; Receipt files remain Appointment-linked only |

## §12 Users, permissions and multi-practice administration

| Brief ref | Requirement (paraphrase) | Disposition | Authority / where |
|---|---|---|---|
| §12 | Más Consultas administrator role (onboarding, commercial config, integration health, permitted metrics; no general sensitive-data access) | Rejected by rev.2 | ADR-0001 (one deployment per Practice) + issue #2 Out-of-Scope (staff roles, multi-tenancy) |
| §12 | Titular Doctor role: agenda, staff, own policies and administrative data | Implemented | The single Doctor owns all dashboard decisions (`CONTEXT.md` `Doctor`); "staff" management does not exist |
| §12 | Reception role: appointments, confirmations, contact and authorized collections | Rejected by rev.2 | Issue #2 Out-of-Scope (staff roles); the Doctor is the only dashboard user |
| §12 | AI agent role: specific operations through restricted tools/permissions | Implemented | ADR-0002: the model only parses into a closed intent set; the application executes booking rules |
| §12 | Technical provider role: temporary, authorized, traceable, least-privilege support access | Operator/legal gate | Operator-only enrollment/deployment procedures in README runbook; no provider support account exists in the application |
| §12 | Individual accounts, strong authentication, secure account recovery, per-practice isolation; new onboarding without separate implementations per Doctor | Rejected by rev.2 | Multi-tenancy and roles rejected by ADR-0001; each new Doctor is a second deployment. Strong Doctor authentication itself is `Implemented` (password+TOTP, `src/modules/doctor/`) |

## §13 Panels, measurement and attribution

| Brief ref | Requirement (paraphrase) | Disposition | Authority / where |
|---|---|---|---|
| §13.1 | Doctor view: pending requests and confirmed appointments | Implemented | Attention-first dashboard (`CONTEXT.md` `Attention Item`, `src/modules/conversation/http.rs` dashboard) |
| §13.1 | Doctor view: realized and cancelled appointments | ADR decision required | Cancelled/past Appointments are listed; "realizadas" as a state requires the §5.2/§8 ADR decision issue |
| §13.1 | Doctor view: absence percentage | ADR decision required | Metrics on No-asistió require the ADR decision issue; no analytics today (issue #2 Out-of-Scope) |
| §13.1 | Doctor view: patients and bookings by channel | Rejected by rev.2 | Issue #2 Out-of-Scope (analytics, campaign/channel attribution) |
| §13.1 | Doctor view: administrative payment state when the module is active | Implemented | Receipt/Payment state in Appointment detail and dashboard payment review (`src/modules/payments/http.rs`) |
| §13.1 | Doctor view: alerts on errors or unattended requests | Implemented | Attention Items include failed sync, lost connection, Late Receipts and Followup Requests |
| §13.2 | Más Consultas view: prospects, requests, confirmed/realized appointments by campaign when measurable | Rejected by rev.2 | Multi-practice platform view rejected by ADR-0001; attribution rejected by issue #2 Out-of-Scope |
| §13.2 | Conversion rates conversation→appointment and appointment→attendance | ADR decision required | Attendance leg requires the ADR decision; conversation→appointment analytics are `Rejected by rev.2` (no analytics) |
| §13.2 | First-response time and human-referral percentage | Rejected by rev.2 | Issue #2 Out-of-Scope (analytics) |
| §13.2 | Causes of lost appointments and cancellation trends | Rejected by rev.2 | Issue #2 Out-of-Scope (analytics) |
| §13.2 | Technical state per practice and active licence volume | Rejected by rev.2 | Multi-practice platform rejected by ADR-0001 |

## §14 Architecture, security and compliance

| Brief ref | Requirement (paraphrase) | Disposition | Authority / where |
|---|---|---|---|
| §14 | Modular documented architecture: official WhatsApp API, AI model provider, secure tool engine, CRM/backend, database, agenda, message queues, reminders, document storage, web panel, observability | Implemented | This application (Axum/SeaORM modules, durable outbox/jobs, Google REST per ADR-0003). Official WhatsApp API element: `Rejected by rev.2` (WAHA/GOWS, `docs/integrations/waha.md`) |
| §14 | Encryption in transit and at rest; restorable backups and recovery plan | Operator/legal gate | TLS/backup/restore are deployment procedures in README runbook (operator); credential encryption at rest is implemented in `src/modules/doctor/crypto.rs`, Google/Doctor-Number crypto modules |
| §14 | Roles/permissions, critical-action audit and cross-practice access controls | Implemented | Single-Doctor authorization boundary + append-only history; cross-practice controls are inapplicable per ADR-0001 (no second practice in a deployment) |
| §14 | Protection against double booking, agent abuse, repeated messages and integration failures | Implemented | Hold exclusion constraint; single-dispatch outbox (uncertain text never resent); bounded retries/recovery in `src/modules/whatsapp/recovery.rs` |
| §14 | Privacy notice, consent and controller/processor definitions | Operator/legal gate | Provisional privacy notice and consent remain legal release gates (README runbook; brief §14 LFPDPPP) |
| §14 | Data minimization toward the AI model; no training of general models on sensitive data without applicable basis/authorization | Implemented | Claude is an optional closed read-only parser with minimized context (`src/modules/conversation/parser.rs`); it cannot decide facts (ADR-0002). Contractual no-training terms with the provider: `Operator/legal gate` |
| §14 | Termination, portability/export, deletion and operational-continuity protocols | Operator/legal gate | Retention/deletion processing is implemented (`src/modules/appointments/retention.rs`); export/termination protocols are operator/legal decisions not defined in the application |
| §14 | Review against LFPDPPP; possible future NOM-004/NOM-024 evaluation for a clinical record | Operator/legal gate | Legal review pending (README gates); clinical record itself is out of MVP scope |

## §15 Beta with three doctors

| Brief ref | Requirement (paraphrase) | Disposition | Authority / where |
|---|---|---|---|
| §15.1 | Beta profiles: Doctor with reception, Doctor without dedicated reception, Doctor with advance payment | Operator/legal gate | Beta staffing/recruitment is an operator decision; the single-Doctor application supports the no-reception and advance-payment (video) flows synthetically |
| §15.2 | New patient consults, chooses a schedule and obtains a confirmed appointment per the rules | Implemented | Synthetic full-router and native local evidence (README changelog); live beta itself is `Operator/legal gate` |
| §15.2 | Two patients try to book the same time; only one appointment is confirmed | Implemented | Invariant row (Hold exclusion) + concurrent-booking tests |
| §15.2 | Doctor blocks a space and the agent stops offering it | Implemented | Blocking Calendars + Holds in availability (`src/modules/appointments/availability.rs`) |
| §15.2 | Reception rejects/modifies a request and the patient receives correct information | Implemented | Doctor dashboard decisions render corrected application messages; "reception" as a separate role is `Rejected by rev.2` (§12) |
| §15.2 | Patient reschedules; calendar and conversation stay synchronized | Implemented | Same-Appointment reschedule (`src/modules/appointments/booking.rs` `request_reschedule`) |
| §15.2 | A medical-urgency message is escalated without automatic diagnosis | Implemented | Out-of-scope input becomes a Followup Request; no diagnosis is generated (ADR-0002) |
| §15.2 | WhatsApp or AI interruption loses/duplicates no requests | Implemented | Durable intake before acknowledgement, idempotent jobs, recovery in `src/modules/whatsapp/` |
| §15.2 | A second practice cannot see another practice's data | Rejected by rev.2 | Multi-tenancy rejected by ADR-0001; there is no second practice in a deployment to test |
| §15.2 | Reminders arrive per authorizations and replies update appointments | Implemented | Reminder + attendance-reply handling in `src/modules/appointments/timers.rs`; sending authorization itself is `Operator/legal gate` |
| §15.2 | An unverified receipt never produces a payment confirmation | Implemented | Invariant row |
| §15.3 | Written approval criteria before the pilot; minimum technical conditions; 30–45 day beta operation | Operator/legal gate | Brief §15 approval is an operator/legal gate; no beta has been run |

## §17 Deliverables requested from Hendrik (technical/delivery)

| Brief ref | Requirement (paraphrase) | Disposition | Authority / where |
|---|---|---|---|
| §17 | Architecture proposal: providers, data structure, integrations, security and backup policies | Implemented | This repository: `docs/adr/0001-0003`, `docs/integrations/*.md`, README runbook/backup sections |
| §17 | Visual process map: patient, agent, reception, Doctor, CRM and appointment states | Operator/legal gate | No standalone visual process-map artifact has been delivered; `CONTEXT.md` and integration docs describe the journeys in text. Delivery remains outstanding |
| §17 | Navigable prototype of the main screens designed for mobile use | Implemented | The server-rendered application itself (desktop + 360px verified surfaces in README evidence) supersedes a throwaway prototype |
| §17 | Scope matrix by phase: included, excluded and dependencies | Implemented | This traceability matrix + issue #2 scope and README capability/gate statements |
| §17 | Milestone schedule and acceptance criteria per delivery | Operator/legal gate | Scheduling/commitment artifact for the operator/commercial parties; issues #3–#27 carry the implementation acceptance records |
| §17 | Development budget and beta conditions | Operator/legal gate | Commercial artifact (§18), not an application deliverable |
| §17 | Recurring-cost table: WhatsApp, AI, infrastructure, storage, support, gateway and others | Operator/legal gate | Commercial artifact (§18.4), not produced by this application work |
| §17 | Support, continuity, technical-access and system-evolution plan | Operator/legal gate | Partially in README operator runbook (backup/restore/rotation); SLA/support plan remains an outstanding operator/commercial decision |
| §17 | Definition of intellectual property, brand use, data, portability and termination | Operator/legal gate | Contractual artifact (§18.11) pending legal formalization |
| §17 | Operational documentation and initial training for Más Consultas, Doctor and reception | Implemented | README operator runbook + `docs/integrations/*.md` are the operational documentation. Formal training sessions: `Operator/legal gate` (not yet delivered) |

## Anexo A Functional journeys

| Brief ref | Requirement (paraphrase) | Disposition | Authority / where |
|---|---|---|---|
| A1 | Patient with internal approval: ask → agent answers → agenda → hold → reception notification → authorization → confirmation → reminders → realized state | Implemented | Implemented through confirmation + reminders (booking, Doctor decision, outbox, timers). Final "realizada" state: `ADR decision required` (§5.2 issue) |
| A2 | Appointment created directly by reception: open CRM → register patient and appointment → agenda occupied → agent stops showing the time → reminders scheduled | Future issue filed | #28 (calendar/manual creation); reminders for such Appointments must reuse the same Hold/History/outbox rules |
| A3 | Patient who does not finish booking: information → no schedule chosen → administrative state saved → follow-up only with authorization and WhatsApp rules → booking or contact closure | Rejected by rev.2 | Conversation state is durable, but consented marketing follow-up automation is not in approved scope (§3.2 row) |
| A4 | Payment-conditioned booking (advanced phase): hold → payment link/instructions → verifiable gateway confirmation or authorized transfer review → confirm or expire the hold → notify the patient | Implemented | Transfer path implemented: Hold + bank instructions + Receipt + Doctor Payment Verification or expiry (`src/modules/payments/`, `src/modules/appointments/`). Gateway-link variant: `Phase 2 per brief §16` (§10.2) |

## Commercial and process record (§§1, 16, 18, Anexo B) — not functional implementation

These passages are negotiation/commercial material. They are recorded here so
they are not mistaken for implemented functions; no application claim is made.

| Brief ref | Passage (paraphrase) | Record |
|---|---|---|
| §1 / Decisiones ejecutivas | SaaS multi-practice product, beta with three Doctors, Hendrik retains the software, non-exclusive distribution, \$800 MXN/month reference, support to be defined | Commercial proposal. Multi-practice SaaS reading is superseded for the current scope by ADR-0001 (one deployment per Practice). Pricing/support remain commercial decisions |
| §16 Phase 0 | Discovery: architecture, flows, prototype, security, budget, acceptance criteria | Process phase; its technical artifacts live in the repo/ADRs (§17 rows) |
| §16 Phase 1 | MVP: WhatsApp AI, knowledge, administrative CRM, agenda, internal approval, reminders, branding, roles, basic panel; no EHR | Phase definition. Current rev.2 delivery covers the single-Practice subset; branding/roles/multi-practice elements are dispositioned in §§11–12 |
| §16 Phase 2 | Advanced automation: online payments, receipts, reconciliation, extended follow-up, advanced metrics/attribution | Source of the `Phase 2 per brief §16` dispositions above (§§10, 13) |
| §16 Phase 3 | Optional evolution: multiple sites/specialists, medical integrations, possible EHR as a separate project | Future/regulatory evolution; requires new decisions (see ADR decision issue for fichas/attendance/metrics) |
| §18.1–18.3 | Commercial alliance, ownership/distribution, two sales modalities plus direct sale | Commercial negotiation; requires written agreement |
| §18.4 | Reference economics (\$800 MXN, wholesale/independent pricing, included vs separate costs, active-Doctor definition, 12-month stability, volume tiers) | Non-binding reference; Hendrik's quotation and joint agreement pending |
| §18.5 | Beta with three Doctors; no monthly licence charging until agreed approval criteria and commercial activation | Operator/commercial gate; no beta run yet |
| §18.6 | Jointly defined technical support (channels, hours, response times, SLA) | Outstanding joint definition |
| §18.7 | Más Consultas advertising of the software; claims must reflect actually operative functions | Commercial activity with a truthfulness constraint |
| §18.8 | Más Consultas technical-commercial support; contributions do not create automatic co-ownership | Commercial/legal record |
| §18.9 | Recurring-revenue scenarios at \$800 MXN (3–100 Doctors) | Theoretical gross figures, explicitly not guaranteed income |
| §18.10 | Data, commercial relationship and continuity (responsibilities, portability, service continuity) | Contractual definitions pending (see §14 operator/legal gates) |
| §18.11 | Contract checklist (licence, pricing, variable charges, IP, beta, support/SLA, privacy, export, updates, termination) | Pending formal contract |
| §18.12 | Expected result; nothing is binding until accepted in writing | Commercial record |
| Anexo B | Decision matrix for the first meeting (stack/hosting, wholesale price, independent price, AI/WhatsApp consumptions, beta schedule/acceptance, support/SLA, brand use, data/security) | Meeting decisions for both parties + legal advice; none are application features |

## Future issues filed from this matrix (Step 5)

Filed 2026-10-20 against `hendrixmar/medical_ai_assistant`; implementation
starts only after each issue's governing decision:

1. Calendar grid views and manual Appointment creation from the dashboard — #28
2. Configurable Reminder sequence including 72-hour Reminder — #29
3. Doctor-approved knowledge answers (FAQ/professional profile) as verbatim templates — #30
4. ADR decision: persistent Patient fichas, Realizada/No-asistió states, and metrics — #31 (`ADR decision required` rows above)
