---
name: Doctor UI — Operate
description: Current Spanish practice administration UI with light neutrals, system type, one blue accent and native controls.
colors:
  surface: "#ffffff"
  background: "#f7f8fa"
  text: "#202b38"
  muted: "#526071"
  accent: "#245a91"
  accent-hover: "#194570"
  line: "#d7dde4"
  control-line: "#748191"
typography:
  body:
    fontFamily: 'system-ui, -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif'
    fontSize: "1rem"
    lineHeight: 1.6
  headline:
    fontSize: "1.75rem"
    fontWeight: 650
    lineHeight: 1.25
  title:
    fontSize: "1.25rem"
    fontWeight: 650
    lineHeight: 1.4
  label:
    fontSize: "1rem"
    fontWeight: 600
  hint:
    fontSize: "0.9375rem"
rounded:
  control: "5px"
spacing:
  small: "0.5rem"
  control-block: "0.625rem"
  compact: "0.75rem"
  regular: "1rem"
  mobile-gutter: "1.25rem"
  roomy: "1.5rem"
  section-gap: "2rem"
components:
  button-primary:
    backgroundColor: "{colors.accent}"
    textColor: "{colors.surface}"
    rounded: "{rounded.control}"
    padding: "0.625rem 1rem"
    width: "100%"
  button-primary-hover:
    backgroundColor: "{colors.accent-hover}"
  button-logout:
    backgroundColor: "{colors.surface}"
    textColor: "{colors.text}"
    rounded: "{rounded.control}"
    padding: "0.625rem 1rem"
  button-logout-hover:
    backgroundColor: "{colors.background}"
  input:
    backgroundColor: "{colors.surface}"
    textColor: "{colors.text}"
    rounded: "{rounded.control}"
    padding: "0.625rem 0.75rem"
    width: "100%"
---

# Design System: Doctor UI — Operate

## Overview

**Creative North Star: "A Spanish task surface, not a branded analytics dashboard."**

This records the built Spanish Doctor surfaces for one Practice: login and settings; the WhatsApp connection; attention-first dashboard; Contact inbox/history; Appointment list, detail and history; protected Receipt review/download; Doctor Number and read-only agenda; and Google Calendar configuration. The pinned conventional Operate direction remains light neutral surfaces, sober blue actions, system type and native forms. It is an implementation record, not a broader medical brand or new component library.

**Key Characteristics:**
- Plain Spanish, task-first hierarchy and truthful operational-state copy.
- Single-column content, generous controls and wrapping mobile navigation.
- Flat surfaces, visible keyboard focus and no decorative motion or imagery.

Sources: `PRODUCT.md`, `src/modules/doctor/views.rs`, `src/modules/practice/views.rs`, `src/modules/whatsapp/views.rs`, `src/modules/conversation/views.rs`, `src/modules/doctor/doctor.css`, and the direction contract in emitted markup. CSS custom-property names are retained in the color tokens; other frontmatter entries describe observed values, not new CSS variables. The minimal `.impeccable/design.json` adds the breakpoint and renderable component examples, not another token authority.

The issue #4 PNGs record an isolated static-shell review only (desktop 1280px; mobile 360px). They do not establish authenticated routing, persistence, provider acceptance or the later integrated Appointment, Contact and Receipt surfaces; the newer native evidence is stated separately below and in `PRODUCT.md`.

## Colors

### Primary

`accent` is the sole blue accent: primary submit, content links, caret, selection background and focus outlines. `accent-hover` darkens primary hover/active states and ordinary link hover. Selection text uses `surface`. There is no secondary or tertiary accent.

### Neutral

`background` is the page canvas; `surface` is used for inputs, the header, logout button and error notice. `text` carries headings, labels and main copy; `muted` carries introductions, hints and explanatory text. `line` separates header and content sections; the stronger `control-line` defines controls and the error notice. Errors use explicit text and a neutral container, not an invented red status palette.

## Typography

All roles inherit the body system-font stack; inputs and buttons inherit the surrounding font. `headline` maps to h1, `title` to h2, `label` to form labels and buttons, and `hint` to field/session help. Navigation's current item and the dashboard link use weight 650. There is no display face, external font, monospace role or uppercase label treatment.

Body copy uses the inherited line height; paragraphs cap at 68ch. Headings and paragraphs wrap long words. Links remain underlined, with a 0.2em offset and 1px stroke, increasing to 2px on hover and for the current navigation item.

## Layout

- **Login:** centered single column, maximum outer width 28rem, horizontal padding 1.25rem and vertical margin 4rem. The introduction follows the title by 0.75rem; the form starts 2rem below preceding content. Each field has 1.5rem bottom spacing; the full-width submit follows both fields. Session help follows by 1rem. No surrounding card.
- **Dashboard/settings:** navigation and main share a 58rem maximum outer width. Navigation has 0.75rem vertical and 1.5rem horizontal padding, wrapping flex layout, and gaps of 0.5rem by 1.5rem. Main padding is 2.5rem 1.5rem 4rem. Sections use a top divider, 2rem top margin and 1.75rem top padding, not a tile grid.
- **Mobile (max-width 36rem):** login top margin becomes 2.5rem; navigation's horizontal padding and column gap become 1.25rem. “Mi consultorio” occupies a full row, with settings/logout wrapping below. Main padding becomes 2rem 1.25rem 3rem. Content retains its reading order; there is no collapsed-menu control.

Outer widths include padding through border-box sizing. Dashboard order stays “Necesita tu atención” before “Tus próximas citas”; “Configuración” remains secondary.

## Elevation & Depth

No shadows, gradients, overlays or elevation scale. White surfaces, a light canvas, thin dividers and whitespace provide separation. Focus outlines communicate interaction, not elevation. There are no transitions or animations; the skip link changes position immediately when focused.

## Shapes

Inputs, buttons and the error notice share gently curved control corners. Borders are 1px: strong neutral on inputs/logout/errors, blue on the primary action, light neutral on structural dividers. Sections and header are plain rectangular regions. No pills, chips, decorative icons or imagery are present.

## Components

### Buttons and links

The filled, full-width “Iniciar sesión” button is the primary action; “Cerrar sesión” is a neutral outlined button. Hover and active share each button's darker/tonal state. Buttons, navigation links, the settings return link, error recovery link and skip link have a 48px minimum height. Ordinary links darken on hover; navigation links retain main text color and strengthen their underline.

Keyboard focus uses a 3px solid accent outline with 4px offset. The error notice gets the same outline on focus. Do not remove these indicators or substitute color alone for current navigation state.

### Native login fields and errors

Visible labels are explicitly associated with controls. Password uses `type="password"`, `autocomplete="current-password"` and `required`. The code is one text field, not segmented boxes: numeric input mode, one-time-code autocomplete, six ASCII digits via pattern and length constraints, and `required`. Help is linked with `aria-describedby`; no scripts intercept paste or replace native inputs. Visible inputs have a 48px minimum height, with padding and shape defined above.

The error notice sits between introduction and form, with 1rem padding and 1.5rem top margin. Emitted markup uses `role="alert"`, `tabindex="-1"` and `autofocus`, and the form references it through `aria-describedby`. Invalid-credential, rate-limit, expired-form and unavailable states have distinct Spanish copy. Expiry includes “Abre un formulario nuevo” linking to `/login`. The supplied invalid/expired screenshots show the focused notice; they do not establish backend error handling.

Login emits a native POST form to `/login` with hidden `csrf_token` and `return_to` fields; logout emits a native POST form to `/logout` with `csrf_token`. These are markup contracts only. The displayed 30-day session sentence is approved copy, not evidence of enforced lifetime.

### Navigation and content regions

Documents declare `lang="es"`, viewport metadata and a page-specific title. Dashboard/settings have a labeled navigation landmark, `aria-current="page"`, and “Saltar al contenido”, hidden offscreen until focused. The skip destination is the main region with `tabindex="-1"`. Each page has one h1; dashboard/settings sections use h2 headings linked by `aria-labelledby`.

The original dashboard placeholders were superseded by the live Attention Item and Appointment renderers. Empty data is distinct from load failure; Appointments and Contacts come from protected application views, not fabricated fixtures. Settings keeps “Volver al panel” and shared logout/navigation with the native configuration forms below. These observed semantics are not an accessibility certification.

### Appointments, Contacts and Receipts — current native surfaces

Keep these surfaces inside the same 58rem light-neutral Operate shell: plain divided lists and sections, system type, one blue action color, wrapped labels, no analytics cards or new palette. The dashboard keeps “Necesita tu atención” before “Tus próximas citas”; Appointment lists, details and append-only history show real records and their factual state. Show Appointment status, payment state and calendar-sync state separately; a Receipt is evidence, never proof of received funds. The Patient name remains on its Appointment, not in a Contact/patient registry.

Contact inbox/history remains scoped to that Contact. Distinguish incoming/outgoing content, timestamps and queued/sent/reported-delivery/read evidence in plain text; transport acknowledgement is not attendance. Protected Receipt review presents the copied image/PDF through an authenticated download route, not a public URL. Native Doctor actions distinguish review, explicit “Confirmar pago recibido”, rejection and Late Receipt handling. Paid-cancellation refund attention offers only the explicit “Marcar reembolso como resuelto” record action: it does not transfer money or undo verified payment.

Preserve the existing accessibility rules: one page h1, labeled navigation and skip link, h2-led sections, visible keyboard focus, associated labels/help, escaped user content, text status rather than color alone, native links/forms, and 48px minimum action targets. Keep the list/detail/history reading order on mobile and allow Contact names, messages, timestamps and notices to wrap. Do not present a clinical history or imply complete accessibility certification. The shared native run verified actual appointment booking/rescheduling and native list/detail/history routes. The actual built image subsequently passed only a constrained synthetic password+TOTP login and four-step Practice setup smoke; the final Receipt mobile/refund/cancel/logout smoke remains in progress and is not claimed.

### Settings steps and native forms (issue #5 extension)

An ordered list links to four GET steps: Datos del consultorio, Servicios, Horarios, and Pagos de video. Each shows stored “Listo” or “Por completar” state; the active link uses `aria-current="step"`, weight and underline rather than a new status color. The list is four columns on desktop and two on mobile. Only the active step's form is rendered, with one POST action and hidden CSRF token; services has an independent save followed by a “Continuar a horarios” GET link. Changing steps does not save edits.

Forms cap at 42rem and reuse the existing field, label, hint, button and focus vocabulary. Native select/textarea controls inherit system type, strong neutral borders, blue focus and a 48px minimum height; textareas resize vertically. Related short fields use two desktop columns and stack at the existing 36rem breakpoint. Each weekday has a legend, explicit status and always-visible interval input; closed-day help asks for blank hours. Service rows are a plain divided list with Editar links, not cards.

The focused error summary links to invalid fields, whose help and inline error are associated through `aria-describedby`. All supplied text is escaped and field values are preserved; numeric errors also repeat the submitted string in help because native number inputs hide malformed values. Timezone has an empty choice and preserves rejected values in an option. CLABE is a digits-only text control, never a number. The renderer supplies no operational defaults. Backend-provided local-time examples explicitly identify the selected zone and are not Appointments. The Conexiones footer links to the WhatsApp connection, Doctor Number verification, and Google Calendar settings views; the links themselves imply no connection status. Payment help distinguishes receipt transit tolerance from the send-by deadline.

Isolated issue #5 evidence: `/tmp/medical-issue5-ui/{details,services,schedule,payments}-{empty,populated,error}-{1280,360}.png`, with synthetic fixtures only. The std-only renderer and browser checks cover escaping, native validation, keyboard/error focus, labels/help, 48px targets, overflow and existing token contrast. These checks do not establish authenticated HTTP behavior, persistence, CSRF enforcement, provider connectivity or a production release.

### WhatsApp connection (issue #6 extension)

The connection view reuses the header, plain divided section, native form actions and neutral focused notice. Ten explicit states distinguish missing configuration, unavailable status, stopped, starting, QR, two passkey steps, connected, failed and unknown. Only stopped renders “Conectar WhatsApp”, a POST form with hidden CSRF token. “Actualizar estado” and “Volver a configuración” remain GET links. Connected copy attributes the report to WhatsApp and shows a supplied phone only in that state. The issue #7 copy update replaces the old unavailable-responses sentence in every state: connection alone neither enables automatic responses nor confirms delivery; the dashboard shows outbound enablement and pending work. Passkey steps explain that completion is outside this page and needs human help; no simulated completion or recovery action is present.

Only the QR state renders the protected same-origin image route, meaningful alternative text, ordered phone instructions and explicit refresh guidance. Its white, strongly bordered image box is 280px square at available widths and shrinks to fit; there is no bundled QR asset, inline image data, polling or new status palette. Phone and instruction text can wrap anywhere. The settings connection link retains a 48px minimum target. Only the QR state additionally renders the “Vincular con un código” section: a native POST form (`/settings/whatsapp/request-code`, hidden CSRF, labeled `tel` input for the international number) and, after a successful request, the pairing code as selectable plain text at the Doctor Number code scale with ordered phone instructions and an expiry/no-persistence note. No other state renders that form or a code, a GET never displays a code, and passkey states remain action-free with completion outside this page.

Isolated evidence: `/tmp/medical-issue6-ui/` contains the std-only `render.rs`, `check.cjs`, `results.json` and `{state}-{1280,360}.png` screenshots for all ten states, long phone/error fixtures, missing phone, a start notice and the updated settings footer. Thirty synthetic browser cases checked form/navigation contracts, escaping, QR exclusivity, notice/keyboard focus, labels, target heights, overflow and token contrast. The QR image is a one-pixel fixture, not a real or scannable code. These checks do not establish authentication, provider connectivity, callback delivery, storage persistence or bot readiness.

### Historical issue #7 attention/privacy renderer checkpoint

The dashboard renderer consumes only the frozen `DashboardView` and typed `AttentionItem` values supplied by the backend. It preserves the existing shell, headings and attention-before-Appointments order. A single plain divided list uses h3 state headings, escaped Contact labels and backend-formatted timestamps; an unresolved Contact label is not converted into a phone. Followup Requests alone have the native “Marcar como atendida” POST action, an escaped UUID path and hidden CSRF token. Shared visible help explains that resolution closes the request and queues a notice, not that a personal conversation or delivery occurred. Failed, uncertain, provider-reported-error and identity-conflict rows have factual explanatory text, with no retry, send or Takeover control. Uncertainty explicitly allows that the message may have sent; a reported error does not erase possible prior delivery/read evidence.

Outbound enablement and actual queued/rate-deferred counts are plain operational sentences, never analytics cards or settings controls. Disabled sending explicitly preserves pending work. An empty queue describes no current pending requests/problems; an empty paginated slice makes no whole-queue claim. Load failure suppresses rows, counts and pagination, shows the existing focused neutral error treatment, and provides a native GET recovery link. Optional previous/next links use `/dashboard?page=N`. Notices, contacts, timestamps, paths and CSRF values are escaped; queue counts and page numbers are typed integers. No scripts, assets, fonts or status palette were added.

Additive CSS retains the 58rem shell and 36rem breakpoint. List entries have 1.5rem vertical padding and neutral dividers, 1rem/650 state headings, and 0.9375rem muted timestamps with tabular numerals. Resolution buttons keep the existing blue action/focus vocabulary and become full width on mobile. Long Contact labels, notices and timestamps wrap anywhere. Pagination wraps as needed, with 48px native link targets.

The public privacy renderer reuses the document/CSS but has no Doctor navigation or authentication forms. Spanish usted copy explicitly marks the notice **PROVISIONAL — no aprobado legalmente**, limits the current purpose to the administrative menu and personal-attention requests, discourages clinical information, and identifies responsible-party, contact and ARCO details as pending. It does not claim consent or legal compliance. Public reachability and legal approval remain separate launch gates.

Isolated evidence: `/tmp/medical-issue7-ui/` contains `render.rs`, `check.cjs`, `check.log`, `results.json`, `detector.json`, generated HTML/CSS and `{state}-{1280,360}.png` full-page plus `{state}-{1280,360}-top.png` viewport screenshots. Twenty explicitly synthetic states at both widths cover empty, disabled, populated, each of the five attention kinds, queued, rate-deferred, load failure, notice, long/escaping input, first/middle/last/empty/full-25-row pagination, privacy and the updated WhatsApp caveat. Two additional `followup-{1280,360}-keyboard.png` images record native keyboard focus. One batched desktop/mobile inspection found no required UI fixes. Standalone std-only `rustc -D warnings` and owned-file rustfmt checks passed; the mechanical detector returned no findings. Forty browser cases passed semantic/escaping checks, no horizontal overflow, 48px targets, existing-token contrast, load-error focus, skip-link navigation, and keyboard submission with the exact native POST/CSRF body. Native pagination and recovery GETs were also captured.

All browser requests were intercepted in Playwright without a server; forms submitted only to the isolated fixture harness. This evidence does **not** establish authentication, authorization, CSRF enforcement, database transitions, backend pagination/order, message delivery, public notice reachability, legal approval or live-provider acceptance. No Cargo build/test was run by the UI worker. The removed obsolete Doctor dashboard's handler/test callers and route integration belong to the backend worker; independent integrated review belongs to the parent.

### Historical issue #8 Doctor Number renderer checkpoint

`src/modules/doctor_number/views.rs` consumes the frozen `DoctorNumberView` and `ChallengeView` with the existing document, navigation and escaping helpers. It preserves the light-neutral/system-font/one-blue Operate surface: saved binding, challenge, then read-only scope in plain divided sections. The Conexiones footer adds the protected `/settings/doctor-number` link without inferring verification status.

Only `Active` renders the code: six contiguous, selectable digits at the existing 1.75rem heading scale in system type with tabular numerals, the challenge's saved Practice destination, and the backend's date/time/zone expiry string. The fresh connection number is separately labeled and never replaces that destination. Expired, exhausted and used states render no code. Saved ownership and verification time remain visible when provider status is unavailable; a database load error suppresses all supplied state, feedback and issuance actions. It retains the focused neutral recovery notice, navigation and logout.

The native issuance POST targets `/settings/doctor-number/challenge` with only hidden `csrf_token`; there is no phone input, QR, clipboard script, external link or auto-refresh. Any supplied prerequisite/throttle block replaces the form with its escaped explanation. Manual refresh is a plain GET, and connection/settings/dashboard links preserve the existing navigation. Copy explains the 10-minute validity, five eligible incorrect guesses (including another contact's), 60-second/hourly issuance limits, resolved sender and Practice account evidence, invalidation of the old code without replacing the old binding, and read-only WhatsApp restrictions. At the #8 renderer checkpoint, agenda and notifications were not yet available. The later full application adds the read-only Doctor agenda and binding-fenced notifications; the verified number still cannot mutate records or by itself prove message delivery.

Isolated evidence: `/tmp/medical-issue8-ui/` contains the actual-source std-only `render.rs`, intercepted Playwright `check.cjs`, logs, `results.json`, generated HTML/CSS, 38 state screenshots at 1280/360, and two native-keyboard screenshots. Nineteen fixtures cover none, prerequisites, unavailable/not-ready, leading-zero active code, old binding, changed Practice destination, active/bound provider failures, expired/exhausted/used, verified, throttled, notice, load failure, long malicious fields and settings navigation. Standalone `rustc -Dwarnings`, scoped renderer rustfmt and all 38 browser cases passed. Checks cover escaping, saved destination preservation, stale-state suppression, native POST/GET/logout targets and form bodies, selection, semantic references, keyboard/error focus, 48px targets, existing contrast and no horizontal overflow. One batched inspection and one confirmation were performed. The detector ran once and reported one off-scale code-size advisory; the fix reuses 1.75rem rather than adding a typography token. It was not rerun.

These artifacts are isolated synthetic renderer evidence, not proof of authentication, CSRF enforcement, persistence, live-provider acceptance, legal approval or delivery. No Cargo or global formatting ran in the UI worker. Backend integration and final global formatting are handed back to the backend owner; independent visual review remains with the parent.

### Historical issue #9 Google settings renderer checkpoint

`src/modules/google_calendar/views.rs` consumes the frozen `GoogleView`, `ConnectionView`, `CalendarChoice`, `PermissionView`, `CheckView` and `CleanupItem` API. It shares the existing document, navigation, escaping helpers, neutral palette, system typography and native form controls. Four plain divided sections cover connection, calendars, permissions/Meet, and retained cleanup. A visible warning and in-page link bring outstanding cleanup forward without conflating it with the current check. This settings page has no test Meet URL, verified account identity or sole-ownership claim; connection/probe state alone does not enable an Appointment or video payment.

The native calendar select contains only backend-supplied management candidates; all supplied calendars remain available as explicitly selected blocking checkboxes, including writer-only entries. Saving has hidden CSRF, generation and selection-version fields, and repeated `blocking_calendar` values. Connect, probe and cleanup-only POSTs use the frozen routes/fields; cleanup IDs are encoded as a single path segment. Empty/dot-only unsafe cleanup IDs suppress the action. No client-side code is added. Block reasons replace their forms; selection also requires a connected, fresh complete list, probe requires a connected state, and cleanup retry requires a connected state plus `can_retry`. A whole-page load error suppresses supplied state, notices and write actions except logout.

Saved calendar metadata remains explicitly historical during an outage. Selection/availability validation, demonstrated event permissions, and successful Meet plus cleanup are separate facts. Checked timestamps become historical when the connection is unavailable; old cleanup warnings remain visible alongside a later passed check. Native refresh never starts another probe. Consent explains the real private, transparent five-minute event and attempted automatic cleanup without promising zero provider notifications. Cleanup-only retry warns that the current account may lack access to the original resource; inaccessible is not treated as deleted. Cleanup pagination uses `cleanup_page`, separate from Contact pagination.

Additive CSS only supplies wrapping remote labels, a native checkbox/label row with a 48px minimum target, fieldset structure, and an independent dashboard summary. `DashboardView` adds only `google_cleanup_count` and `google_load_error`. Its Google block sits under “Necesita tu atención” and links to settings without manufacturing a Contact or Followup Request. Google load failure retains legitimate Contact actions; Contact load failure leaves Google warnings visible. A supplied prior cleanup count remains explicitly historical during a Google load error. Existing Contact counts and pagination are unchanged.

Isolated evidence: `/tmp/medical-issue9-ui/` contains the actual-source std-only `render.rs`, intercepted Playwright `check.cjs`, `compile.log`, `rustfmt.log`, `check.log`, `results.json`, `detector.json`, rendered HTML/CSS, full-page and top screenshots at 1280/360, keyboard images, and inspection contact sheets. Forty-two synthetic fixtures produce 84 responsive browser cases: 34 Google states, seven independent dashboard combinations, and settings navigation. Thirty-eight native POST/GET checks cover exact form bodies (including multiple/no blocking values, empty/current/stale expected probe pointers and versions), cleanup-only targets, logout, refresh and independent pagination. The matrix checks escaping, form suppression, linked semantics, autofocus/keyboard focus, 48px targets, existing-token contrast and horizontal overflow. One batched visual inspection found no material UI correction; one bounded confirmation added explicit keyboard submission evidence. No polishing loop was run. Standalone `rustc -Dwarnings`, scoped rustfmt and the browser matrix passed; the detector ran once with no findings. Existing global styling was not rewritten.

All browser traffic is intercepted locally with synthetic DTOs; no browser preview is published. These results establish rendering and native request serialization only, not OAuth, authenticated routing, Origin/CSRF enforcement, stale-form rejection, storage, provider behavior or live Google acceptance. Backend constructor updates, integrated compilation/tests and independent source/browser review remain with the backend owner and parent. No Cargo, global formatting, real credentials, provider requests, or production test hooks were used by this UI worker.

## Do's and Don'ts

### Do:
- **Do** preserve plain Spanish, native form semantics, visible labels, linked help, keyboard focus and paste/autofill support.
- **Do** preserve the light-neutral/system-font/one-blue-accent direction and the desktop/mobile reading order.
- **Do** distinguish unavailable functionality from an empty live queue, and rendered UI contracts from verified backend behavior.

### Don't:
- **Don't** invent a broader medical brand, Doctor identity, provider endorsement, imagery or decorative dashboard data.
- **Don't** replace this narrow shell with analytics cards, new controls, new status palettes, motion or a broader design system.
- **Don't** treat screenshots, hidden form fields or session copy as proof of authentication, authorization or other backend functionality.
