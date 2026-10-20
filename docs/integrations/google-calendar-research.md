# Google Calendar REST — issue #9 research and operator gates

Evidence date: **2026-10-14**. Contract: approved issue #9, read in full from
`/tmp/medical-issue9-current.json`, [domain vocabulary](../../CONTEXT.md), and
[ADR 0003](../adr/0003-google-calendar-rest-not-mcp.md). One Practice, one
operator-owned OAuth client/account, an owned Booking Calendar, selected Blocking
Calendars, direct REST, **no MCP**. This is research, not implementation or live acceptance.
The supplied #8 baseline is 146 locally validated tests; it was not rerun here.

Only public official Google documentation and discovery metadata were fetched.
No credentials, account APIs, token endpoints, Console configuration, real events,
deployment, or tracker operations were accessed. Raw source copies, readable extracts,
URL/SHA-256 manifests and a selected schema extract are in
`/tmp/medical-google-source/`. Calendar discovery revision **`20261005`**, SHA-256
`f45a60fde72b589a0e5993d47dc1952226821ff1b069ab3b4763514b150fef9b`.[discovery]
Those temporary copies are local evidence, not required runtime artifacts.

**Evidence boundary:** HTTP/JSON examples below are deliberately synthetic,
source-derived DTO projections, not captured Google responses. Source facts are cited;
application safeguards and unresolved decisions are distinguished from provider guarantees.
Synthetic full-router/real-Postgres tests and an explicitly authorized real
create/read/delete gate remain separate. Passing this check does not launch patient
video, payment, delivery, or future Appointment reconciliation (#13/#15/#23).

## Approved authorization contract

Request exactly this case-sensitive, space-delimited set of full URIs, with no aliases,
substitutions, OpenID scopes or broader example scopes:[auth][discovery]

```text
https://www.googleapis.com/auth/calendar.events.owned
https://www.googleapis.com/auth/calendar.freebusy
https://www.googleapis.com/auth/calendar.calendarlist.readonly
```

| Approved scope | Documented reach / method evidence |
| --- | --- |
| `calendar.events.owned` | See/create/change/delete events on calendars **you own**; explicitly accepted by `events.insert`, `events.get`, `events.delete`. Not blanket event access on every shared calendar.[auth][insert][get][delete] |
| `calendar.freebusy` | Described as “View your availability in your calendars”; explicitly accepted by `freebusy.query`, whose input supports multiple calendar/group IDs. Do not replace with the separately described `calendar.events.freebusy` (“calendars you have access to”). Scope acceptance does not guarantee access to every selected shared calendar.[auth][freebusy] |
| `calendar.calendarlist.readonly` | Read subscribed CalendarList entries, including `calendarList.list/get`; cannot subscribe the Doctor to missing calendars or edit their properties/ACLs.[list][list-resource][discovery] |

The create-events tutorial asks for broad `calendar`; the method references and discovery
explicitly support the narrower approved `calendar.events.owned`. Follow those contracts,
not the tutorial's sample scopes. Do not add `calendars.readonly`, settings, Meet REST,
userinfo or ACL scopes to obtain convenient metadata.[create][insert][discovery]

### Authorization, callback and code exchange

Use a **Web application** client. Google's web-server flow supports confidential client
credentials and server-maintained state. Authorization is browser navigation to
`https://accounts.google.com/o/oauth2/v2/auth`; code exchange and refresh use a server-side
form POST to `https://oauth2.googleapis.com/token`.[web][overview]

Decoded authorization-parameter DTO (URL-encode values, particularly the three-scope string):

```text
client_id              = <operator Web application client ID>
redirect_uri           = <exact registered private HTTPS callback>
response_type          = code
scope                  = <the three full URIs above, joined with spaces>
access_type            = offline
prompt                 = consent select_account
state                  = <unique random opaque session-bound value>
code_challenge_method  = S256
code_challenge         = <base64url(SHA256(ASCII(code_verifier))), no padding>
```

- `offline` requests a refresh token; it does **not** promise one in every response.
  Google's guide says refresh tokens are normally returned on first authorization.
  `prompt=consent` requests re-consent; `select_account` displays account selection;
  the values can be combined with a space. `none` cannot be combined with them.
  `login_hint` is only a hint, not account verification. Consent/account selection is
  appropriate to explicit Conectar/Volver a conectar, not an automatic redirect loop.[web]
- **PKCE evidence:** Google's public issuer metadata advertises `plain` and `S256`;
  its native-app guide documents a fresh high-entropy 43–128-character verifier using
  `[A-Za-z0-9._~-]`, S256 calculation and `code_verifier` in exchange. Use S256, retain
  the verifier server-side with the attempt, and do not replace state or confidential
  client authentication with PKCE.[oidc-discovery][native]
  **Documentation gap:** the current web-server parameter tables omit PKCE, while the
  shared issuer advertises it. This supports protocol support, not proof that this
  uninspected Web client is configured/accepted correctly. Preserve this as a later
  authorized interoperability gate; do not silently remove PKCE on a failed exchange.
- Google returns state in the web callback **query**, on consent or denial. Validate
  state **before handling either code or error**. Google documents `access_denied`,
  `admin_policy_enforced`, `org_internal`, `disallowed_useragent`, `invalid_request`,
  `redirect_uri_mismatch`, `invalid_client`, `deleted_client` and `invalid_grant`;
  some errors are shown by Google rather than delivered to the callback.[web]
- **Required application controls:** authenticated Doctor + Origin/form CSRF on initiation
  and selection changes; bind the random attempt to that live Doctor session, intended
  client/redirect and connection generation; expire it and atomically consume it once.
  Reject absent/mismatched/replayed/duplicate query values and ambiguous code+error
  before exchange. A Google redirect is not an ordinary same-origin form POST: use the
  session/state boundary for the callback, including the expired/logged-out-session case.
  Browser state must not contain tokens or the PKCE verifier. These enforce #9 atop
  Google's state guidance, not additional Google identity claims.[web][best]

```http
POST /token HTTP/1.1
Host: oauth2.googleapis.com
Content-Type: application/x-www-form-urlencoded

client_id=<encoded-client-id>&client_secret=<encoded-secret>&code=<encoded-code>&redirect_uri=<encoded-exact-callback>&grant_type=authorization_code&code_verifier=<encoded-verifier>
```

The web guide currently labels `client_secret` optional in its generic REST table;
Google's overview says a web-server application requires a secret. Keep the approved
confidential Web client's secret server-side and supply its configured authentication;
public discovery advertises `client_secret_post` and `client_secret_basic`. Do not treat
that generic optional label as permission to downgrade client authentication.[web][overview][oidc-discovery]

Example successful exchange body; `refresh_token` and `refresh_token_expires_in` are
optional in the parser. The latter is documented for time-based grants, not a universal
refresh lifetime. `expires_in` is seconds; token type is `Bearer`. Ignore unknown fields,
validate required known fields and bounds, and never log the body.[web]

```json
{
  "access_token": "SYNTHETIC_ACCESS_A",
  "expires_in": 3600,
  "token_type": "Bearer",
  "scope": "https://www.googleapis.com/auth/calendar.events.owned https://www.googleapis.com/auth/calendar.freebusy https://www.googleapis.com/auth/calendar.calendarlist.readonly",
  "refresh_token": "SYNTHETIC_REFRESH_A"
}
```

Synthetic callback shapes: `?code=SYNTHETIC_CODE&state=SYNTHETIC_STATE` and
`?error=access_denied&state=SYNTHETIC_STATE`. A token failure may be
`400 {"error":"invalid_grant","error_description":"..."}`; the description is
untrusted diagnostic data, not safe display/log content. During **code exchange**,
`invalid_grant` can mean an invalid code, so it must not automatically invalidate an
unrelated existing connection.[web]

### Granted scopes, identity and refresh safety

1. **Read the token response's `scope`, not just the requested scope or callback.**
   Split on spaces and compare whole case-sensitive tokens. Users may grant only some
   scopes; absent required permissions disable related operations. Google notes that
   some APIs normalize scope names; these sources establish no replacement alias for
   our three approved Calendar URIs. A missing/invalid grant scope response is not proof
   of all permissions. New-grant validation should fail closed.[web][overview]
2. `include_granted_scopes=true` combines previously authorized scopes **across clients
   in the same project**, can add unexpected scopes, and can trigger warnings/errors.
   Do not copy it from Google's examples for this fixed-scope slice: omit it rather than
   requesting automatic union. This is **not** evidence that omission erases historical
   grants. Inspect actual returned scopes. **Decision:** whether extra granted scopes
   are rejected; conservative default is reject/promote operator remediation, never use
   them, and never silently revoke a whole project to “fix” them.[web]
3. **There is no verified Google account identity in this scope contract.** Google
   authentication uses an ID token and OpenID scopes; none are approved here. Do not
   infer a verified email/`sub` from `login_hint`, `authuser`, calendar ID/summary,
   `dataOwner`, organizer email, or an unexpected ID token. Calendar metadata can label
   the resource but is not an OIDC identity assertion. Do not call userinfo/tokeninfo
   to compensate.[openid][auth][list-resource]
4. Consequently, a **new authorization/reconnect without a refresh token must never
   inherit the old connection's refresh token**: the Doctor might have selected another
   account. Require a complete fresh offline credential set or leave reconnect incomplete;
   preserve old cleanup/settings records without pretending they belong to the new grant.
   This is an application safety consequence, not Google's ordinary same-grant refresh rule.
5. A refresh POST uses `grant_type=refresh_token`, `refresh_token`, client ID and the
   confidential client's authentication. Google's successful refresh example returns
   `access_token`, `expires_in`, `scope`, `token_type` **without a refresh token**.
   Preserve the old refresh token only within that same credential generation. If an
   actual response includes a replacement, persist it atomically with the new access
   token/expiry; missing is not deletion, empty/malformed is not a valid replacement.
   Google's cited docs do **not** promise rotation on every refresh, single-use refresh
   tokens, or an overlap/grace period.[web]
6. **Application concurrency risk:** simultaneous refreshes can overwrite newer token
   state; a late refresh can overwrite a reconnect or resurrect a revoked connection.
   Serialize refresh per connection, recheck freshness after acquiring ownership, and
   condition persistence on the same credential generation/version. Avoid long DB locks
   around HTTP. Persist before enabling dependent operations; bound retries and do not
   claim recovery if the provider rotated a token whose response was lost. These are
   local safety requirements, not a documented Google concurrency guarantee.
7. Use returned expiry, allowing a small local clock-skew margin rather than assuming
   all access tokens last one hour. Distinguish transient transport/5xx failures from
   invalid authorization. Refresh `invalid_grant` means the grant cannot currently be
   used: stop operations and offer Volver a conectar, not endless refresh. A Calendar
   401 can justify one coordinated refresh; persistent 401 is not connectivity.[web][errors]
8. Refresh tokens can be invalidated by revocation, six months of disuse, token limits,
   time-based access or policy restrictions. Google's password-change example applies
   to **Gmail scopes**, not necessarily these Calendar scopes. Its `invalid_rapt`
   discussion concerns **Cloud Platform session controls**; don't label every Calendar
   `invalid_grant` that way. The documented **100 refresh tokens per account/client**
   limit silently invalidates the oldest and is distinct from the OAuth user cap.[overview]
9. Revocation affects the user's project-wide grants/all clients, not just one local
   token. It can take time to take full effect; neither instantaneous remote revocation
   detection nor immediate invalidity is guaranteed. Suspend locally when revocation
   is known; do not add Cross-Account Protection or revoke calls to this slice without
   approval. Do not automatically revoke an old token after same-project reconnect:
   that can invalidate the newly acquired grant too. Revocation can also make cleanup
   impossible.[web][policies]

Google requires encrypted tokens at rest and secure client-secret handling.[policies][best]
For #9, operator-managed stable encryption key material must be supplied privately;
no generated production secret or default belongs in this note. Keep access/refresh
tokens and client secrets server-side, outside browser responses/cookies, WhatsApp,
LLM input, source and logs. Missing/malformed required key material must fail safely;
unreadable ciphertext cannot become “connected.” Record key identifier/format/provisioning
requirements during implementation; key recovery remains #26.

## Policy claims: inherited assumptions versus current documentation

| Ticket shorthand | Current official evidence / boundary |
| --- | --- |
| External / In production / unverified for v1 | External and publishing status are separate settings. In production is not verification. Personal-use apps under 100 users can be exempt from verification; the OAuth policy defines personal use as unshared or fewer than 100 people personally known to the developer. This is consistent with a narrow operator-owned deployment, **not a blanket commercial/public-launch exemption**.[audience][exemptions][policies] |
| “Unverified warning once” | Official docs describe warning screens for unapproved sensitive/restricted scopes and click-through for exempt personal use. They do **not** promise exactly one warning per user, client, reconnect or lifetime. Report the warning without promising frequency or successful bypass.[unverified][exemptions] |
| “Testing = 7 days” | External Testing authorizations expire seven days after consent, including offline refresh tokens. The exception is only basic name/email/profile/OpenID/sign-in access; our Calendar scopes do not qualify. In production removes this particular Testing limitation, **not every possible expiry/revocation**.[audience][overview] |
| “100-user cap” | Distinguish Testing's 100 listed test users from the lifetime-project cap of 100 new users authorizing unapproved sensitive/restricted scopes after the unverified screen. The latter cannot be reset; verified approved scopes are not subject to it. Google explicitly reserves app-specific quota adjustments. This is not a guaranteed universal 100-user entitlement.[audience][unverified] |

**Operator policy decision:** Google production-readiness guidance requires verification
for production sensitive scopes, owned/verified domains and a public homepage; the OAuth
policy separately defines personal-use exceptions. Console's **In production** label does
not by itself settle whether this deployment qualifies for those exceptions. Record the
operator's applicable category and current Console scope classification. Do not promise
that the chosen account, warning bypass or future policy will permit this setup.[production][policies][exemptions]

## Calendar selection and free/busy contracts

### CalendarList, ownership and permissions

`GET /calendar/v3/users/me/calendarList?showHidden=true&showDeleted=false&maxResults=250`
with Bearer auth is supported by the exact list scope. Default page size is 100, maximum
250. Follow opaque `nextPageToken` until absent; an incomplete/failed traversal is not
an authoritative absence. Bound page count, total entries/bytes/deadline and detect
repeated tokens. Hidden defaults false and is normally only emitted when true;
`selected` means visible in Google's UI, **not** selected by this application. `deleted`
means removed from CalendarList, not proof that the calendar itself was destroyed.[list][list-resource]

Fresh `calendarList.get` uses `/calendar/v3/users/me/calendarList/{calendarId}` and the
same scope. Do not rely on incremental CalendarList sync or unchanged ETags to detect
permission changes: Google says read-only properties/ACL changes may not appear in sync
and may not change the ETag. Calendar sharing no longer automatically inserts an entry
in CalendarList; missing shared calendars require operator subscription, not an app
write using this readonly scope.[discovery][list][version][sharing]

```json
{
  "kind": "calendar#calendarList",
  "nextPageToken": "SYNTHETIC_PAGE_2",
  "items": [
    {
      "id": "doctor@example.invalid",
      "summary": "Consultorio",
      "primary": true,
      "accessRole": "owner",
      "timeZone": "America/Mexico_City",
      "conferenceProperties": {"allowedConferenceSolutionTypes": ["hangoutsMeet"]}
    },
    {"id": "shared@example.invalid", "summary": "Bloqueos", "accessRole": "freeBusyReader", "hidden": true}
  ]
}
```

**Important current-source nuance:** `accessRole="owner"` now explicitly means
**manager** access (writer plus managing access), and **is different from the single
calendar data owner**. Multiple accounts may have the owner role. `dataOwner` is an
email present only for secondary calendars; `primary=true` is the authenticated user's
primary calendar, for which they always have owner access.[list-resource][sharing][discovery]

Require fresh `owner` as the approved Booking Calendar's permission filter, but do not
claim this proves sole data ownership or exact `events.owned` enforcement. The cited
scope description says “calendars you own” without resolving this manager/data-owner
edge. **Decision/live gate:** confirm the intended meaning of “owned” for secondary or
manager-shared calendars and test the selected resource with the approved scopes; do not
silently broaden scopes, accept a writer-only calendar, or limit selection to primary
without approval. A successful probe proves operations on that calendar, not an OIDC
account identity or legal ownership.

For shared calendars, `writer` grants event read/write including private details;
`writerWithoutPrivateAccess` excludes private event details/modification; `reader`
can see non-private details and busy/private placeholders; `freeBusyReader` only busy
time. ACL permissions and OAuth scope restrictions both apply. Thus shared write
permission alone does **not** establish Booking Calendar eligibility under
`events.owned`; neither shared writer nor reader justifies event-detail reads for
Blocking Calendars with the approved scope set. Use free/busy instead.[sharing][auth][list-resource]

`conferenceProperties.allowedConferenceSolutionTypes` containing `hangoutsMeet` is
preflight support metadata, **not proof of a successfully created video conference**.
Missing/unsupported metadata is not capability success. Use CalendarList metadata,
not an unauthorized settings/Calendars call.[list-resource][create]

### Blocking Calendars

`POST /calendar/v3/freeBusy` accepts `calendar.freebusy` explicitly. Query **explicit
selected calendar IDs** and a finite RFC3339 window; no need to introduce group selection.
Maximum calendar expansion is 50; group expansion maximum is 100. Busy starts are
inclusive and ends exclusive. HTTP 200 can contain per-calendar/group errors; never
interpret an error, omitted selected result or incomplete page/chunk as free time.[freebusy]

```json
{
  "timeMin": "2026-10-20T09:00:00-06:00",
  "timeMax": "2026-10-20T10:00:00-06:00",
  "timeZone": "America/Mexico_City",
  "calendarExpansionMax": 50,
  "items": [{"id": "doctor@example.invalid"}, {"id": "shared@example.invalid"}]
}
```

```json
{
  "kind": "calendar#freeBusy",
  "timeMin": "2026-10-20T15:00:00Z",
  "timeMax": "2026-10-20T16:00:00Z",
  "calendars": {
    "doctor@example.invalid": {"busy": [{"start": "2026-10-20T15:00:00Z", "end": "2026-10-20T15:30:00Z"}]},
    "shared@example.invalid": {"errors": [{"domain": "global", "reason": "notFound"}]}
  }
}
```

The schema also allows `groups: {"<group-id>": {"errors": [...], "calendars": [...]}}`.
Documented reasons include `groupTooBig`, `tooManyCalendarsRequested`, `notFound`,
`internalError`; Google may add others. An empty busy list without errors is valid;
unknown errors fail closed. Revalidate each selected calendar under the actual grant.
The differing descriptions of `calendar.freebusy` and `calendar.events.freebusy` do
not establish a universal shared-calendar availability guarantee: method acceptance
is established, specific shared-calendar success is an outstanding gate.[freebusy][auth]

## Video capability: real event, asynchronous conference, cleanup

### Create/read DTOs and IDs

Use `POST /calendar/v3/calendars/{encoded-calendar-id}/events?conferenceDataVersion=1&sendUpdates=none`.
`conferenceDataVersion` defaults to 0, which ignores submitted conference data;
1 enables `createRequest`. A test event must have start/end, no attendees, no recurrence,
no Patient data; suppress its own reminders and use a transparent/private event to
minimize disruption. `sendUpdates=none` is not proof of zero mail/side effects: Google
warns some notifications can still occur. This remains a real, operator-authorized
mutation.[insert][event]

```json
{
  "id": "9f10a88d14984804a0a17aa4e5b95032",
  "summary": "Prueba de conexión de video — eliminar",
  "start": {"dateTime": "2026-10-20T09:00:00-06:00", "timeZone": "America/Mexico_City"},
  "end": {"dateTime": "2026-10-20T09:05:00-06:00", "timeZone": "America/Mexico_City"},
  "transparency": "transparent",
  "visibility": "private",
  "reminders": {"useDefault": false},
  "extendedProperties": {"private": {"maPurpose": "video-capability-probe", "maProbe": "SYNTHETIC_UNIQUE_PROBE", "maInstance": "SYNTHETIC_DEPLOYMENT"}},
  "conferenceData": {"createRequest": {"requestId": "SYNTHETIC_UNIQUE_CONFERENCE_REQUEST", "conferenceSolutionKey": {"type": "hangoutsMeet"}}}
}
```

- **Event `id`:** 5–1024 characters, lowercase `a`–`v` and digits `0`–`9`
  (base32hex alphabet), unique per calendar. Hyphenated UUID text and arbitrary base64
  are invalid; UUID hex without hyphens is a fitting example. It is not `iCalUID`.
  Keep the chosen ID durably before dispatch.[event]
- **Conference `requestId`:** separate client-generated unique string; regenerate for
  each **new** conference request. If the same as the previous request, Google ignores
  the request. No specific alphabet/length/idempotency lifetime is documented in the
  captured schema. Do not apply event-ID rules to it. Preserve the same IDs/body for
  retrying one uncertain insert; don't create new IDs to dodge a collision.[event][discovery]
- A duplicate caller event ID normally produces `409` with reason `duplicate`.
  **Not exactly-once:** the Event resource explicitly says distributed ID collisions
  are not guaranteed to be detected at creation. The version guide says insert with
  caller ID succeeds only when absent; ADR 0003 uses “idempotent.” Retain the
  caller-ID strategy, but interpret these as retry-risk reduction, not an absolute
  exactly-once guarantee. Do not update/delete a 409 collision without ownership
  evidence.[errors][event][version][create]

`GET /calendar/v3/calendars/{calendar-id}/events/{event-id}` returns Event JSON;
there is **no `conferenceDataVersion` parameter on GET or DELETE** in discovery.
Polling only reads that exact event. The following are projections of possible
conference payloads, not a requirement that all other Event fields disappear:[get][event][discovery]

```json
{
  "id": "9f10a88d14984804a0a17aa4e5b95032",
  "conferenceData": {"createRequest": {"requestId": "SYNTHETIC_UNIQUE_CONFERENCE_REQUEST", "conferenceSolutionKey": {"type": "hangoutsMeet"}, "status": {"statusCode": "pending"}}}
}
```

```json
{
  "id": "9f10a88d14984804a0a17aa4e5b95032",
  "conferenceData": {"createRequest": {"requestId": "SYNTHETIC_UNIQUE_CONFERENCE_REQUEST", "status": {"statusCode": "failure"}}}
}
```

```json
{
  "id": "9f10a88d14984804a0a17aa4e5b95032",
  "conferenceData": {
    "createRequest": {"requestId": "SYNTHETIC_UNIQUE_CONFERENCE_REQUEST", "status": {"statusCode": "success"}},
    "conferenceSolution": {"key": {"type": "hangoutsMeet"}, "name": "Google Meet"},
    "entryPoints": [{"entryPointType": "video", "uri": "https://meet.google.com/abc-mnop-xyz"}]
  }
}
```

**The URL above is Google's own documentation example**, not a generated or observed
conference; do not open it as part of testing. In production, take the exact URI from
this probe's actual `entryPoints`, never assemble one from its event ID, conference ID,
meeting code, `htmlLink`, label or a URL template.[meet-space][event]

`pending` means processing; `success` means entry points populated; `failure` means
no entry points. Failed creation leaves `conferenceSolution`/`signature` unset;
signature is optional while pending. Failure can therefore be **HTTP 200 Event JSON**,
not an HTTP error. Missing/unknown status, success without a usable video entry point,
and a poll deadline do not pass the capability gate.[event][create]

### URL validation and bounded operations

Calendar's generic schema allows video HTTP or HTTPS, URI length ≤1300 characters,
and zero or one video entry point (phone/sip/more are different types). It does not
provide a comprehensive host allowlist. The Meet Space documentation specifically
identifies **`https://meet.google.com/`** as the meeting URI authority and documents
meeting-code format `[a-z]+-[a-z]+-[a-z]+`, not a universal fixed 3-4-3 length.[event][meet-space]

**Conservative application policy:** require success + `hangoutsMeet` + exactly one
nonempty video URI; parse it, require HTTPS and exact `meet.google.com`, no userinfo,
unexpected port, fragment, controls or deceptive host suffix. Validate a usable meeting
path without constructing a URL; treat unfamiliar path/query forms as unsupported pending
review rather than rewriting them. Do not trust `label`, `iconUri` or HTML notes, fetch
the join URL server-side, or follow a redirect to “validate” it. `meet.google.com.evil`,
`meet.google.com@evil` and `javascript:` fail. Deprecated `eventHangout`/
`eventNamedHangout` and third-party `addOn` are not this new-Meet capability. The schema's
`http://meet.google.com` descriptive link is not a reason to accept insecure runtime
join links. This is intentionally stricter than the generic Calendar DTO, not a claim
that Google guarantees all future URI forms.[event][meet-space]

**Bounded client/probe requirements (implementation must choose and test numeric limits):**
fixed Google HTTPS API origins, verified TLS, bearer auth header, encoded path segments,
no redirects, finite connect/request/body-size limits, explicit maximum poll attempts and
elapsed time, bounded exponential backoff/jitter, and a separate finite cleanup budget.
Never poll by updating the event or issuing fresh conferences. Retry documented transient
rate-limit 403/429 and backend errors selectively, not all 403s. Honor backoff within the
overall deadline. Google's docs recommend exponential backoff but give no conference-ready
SLA, maximum read-after-insert lag or universal linearizability guarantee.[errors][get][create]

An insert timeout/reset/malformed success body or failed local commit can occur **after
Google created the event**. Record the intended connection/calendar/event/marker before
sending; retain “outcome unknown,” then bounded GET by that ID and verify markers. A 409
alone does not establish it is our event. A transient GET 404 is not permission to create
another event with a fresh ID. No retry after switching accounts; no unbounded transport,
library or proxy automatic replay. Provider asynchronous processing/retries and network
uncertainty are not canceled by a local timeout; the sources specify no cancellation or
retry schedule for pending conference generation.[create][event][errors]

### Own-event identification and deletion outcomes

`extendedProperties.private` is private to the **calendarId/eventId copy**, not a secret,
immutable ownership certificate. Keys over 44 characters are silently dropped; values
over 1024 are truncated. Use short application-purpose, deployment and random probe markers
plus the durable exact event/calendar/connection-generation record. GET and verify those
markers before deleting, especially after uncertainty/409. Summary prefix, time range,
`creator.self`, `organizer.self` or ID prefix alone are insufficient. Markers may be edited
or copied; a mismatch/missing marker means retain work for operator review, not delete
unrelated data. Do not search by summary and bulk-delete; private-property searches are
not a replacement for the exact stored target.[extended][event]

Delete only the stored concrete calendar/event, never a newly resolved `primary` alias
following reconnect. `If-Match: <fresh etag>` protects against edits between verification
and DELETE; 412 requires a fresh read/revalidation, never unconditional force-delete.
Permission metadata can change independently of cached CalendarList ETags.[delete][version]

| Result / synthetic fixture | Meaning and safe handling |
| --- | --- |
| `204 No Content`, empty body | Successful deletion acknowledgement; don't require JSON. The Calendar DELETE reference promises an **empty response**, and discovery has no response schema; neither enumerates a mandatory numeric success code. Use 204 in synthetic tests without presenting it as a captured response or the only documented possible success.[delete][discovery] |
| `410` + `error.errors[].reason="deleted"` | Google's documented already-deleted event result; no further deletion needed for the known own target. Not interchangeable with every 410: `fullSyncRequired`/`updatedMinTooLongAgo` describe other operations.[errors] |
| `404` + `reason="notFound"` | Can mean nonexistent event **or calendar inaccessible to this caller**. Do not translate directly to “deleted.” Retain an unresolved cleanup item unless separate trustworthy evidence establishes deletion. Fresh calendar access alone cannot retroactively prove the uncertain DELETE ran.[errors] |
| GET `status="cancelled"` | Nonrecurring events represent deleted events, but GET can retain tombstones; only `id` is guaranteed and tombstones eventually disappear. Moving an event can also leave a cancelled copy. Reconcile only an already identified own probe; don't require markers to survive deletion or infer all copies/conferences are gone.[event] |
| 401, permission 403, refresh `invalid_grant`, lost key, account switch | Cleanup blocked, not complete. Keep the original target/context visibly unresolved. Reconnect may regain access; a different account may never do so. Never carry old credentials into a new grant or bypass scope/permissions for cleanup.[errors][web] |
| Timeout/5xx after DELETE | Outcome uncertain; bounded read/retry of the same verified target, subject to current authorization/ETag. If ambiguity remains, retain it; don't claim success.[errors][version] |

Example standard error envelope (provider message is deliberately not trusted):

```json
{
  "error": {
    "errors": [{"domain": "global", "reason": "duplicate", "message": "The requested identifier already exists."}],
    "code": 409,
    "message": "The requested identifier already exists."
  }
}
```

For a delete-already-done fixture, use code 410/reason `deleted`/message
`Resource has been deleted`; for ambiguous absence, 404/`notFound`/`Not Found`.[errors]

Attempt cleanup after success, conference failure, invalid URI, deadline or subsequent
persistence failure whenever a known/possibly-created own event can safely be addressed.
Persist unresolved work before losing its context; show an Attention Item. Do not mark
video ready until both actual successful conference retrieval **and** satisfactory test
event cleanup complete. A cleanup-only retry is not a new capability probe. Calendar or
connection changes invalidate the old capability result. Deleting a Calendar event is
not documented here as immediate destruction of its Meet space/join URI; don't promise
that stronger effect or add Meet API operations.[delete][event][meet-space]

## Private-browser redirect, logging and launch gates

Google requires an exact registered redirect match, including scheme, case and trailing
slash (and the configured hostname, port and path). Web redirects require HTTPS except
localhost/loopback; non-loopback raw IP hosts are forbidden. Domains must have public-suffix
TLDs; no wildcards, fragments, userinfo, path traversal or open redirects. Native-app
loopback/random-port rules are **not** permission to use that client type for this server
web app.[web][native][overview]

For this SSH/Tailscale setup, the browser is on the ThinkPad: `localhost` would refer to
the ThinkPad, not the server. A prospective callback is the operator-approved exact
`https://<private-hostname>:<https-port>/<callback-path>` reachable by that browser, with
valid TLS and private routing. No URL/port has been selected or published in this research.
Do not register a `100.x` raw-IP callback, use plain HTTP on the HSTS hostname, or expose
Funnel/public ingress merely to make OAuth work. The web code flow returns via browser
redirect; the server separately needs outbound Google HTTPS.[web]

**Hostname/policy gate:** an authorized HTTPS Tailscale hostname is not proof of Google
Console acceptance or domain-verification eligibility. Google's policy permits domains
owned/authorized/licensed to the operator; production verification guidance requires
verifiable ownership and recommends a custom domain for shared-host providers. Whether
this exact `*.ts.net` hostname can meet the applicable verification requirements was not
established. If needed, the operator must decide on an owned custom HTTPS hostname with
private routing/certificates or clarify the personal-use exception. Do not assert the
Console is configured, register domains, or change publishing status.[policies][production][web]

Google explicitly warns that callback URLs expose authorization codes to page scripts and
Referer recipients, and recommends processing then redirecting to a clean URL. Keep the
callback response free of page resources, redirect to a fixed protected settings URL, use
no-store and a callback-specific restrictive referrer policy. Do not unnecessarily change
the existing native form pages' Origin/referrer behavior. Suppress/redact the **whole
callback query** in HTTP access logs, reverse-proxy logs, tracing/error capture and request
debugging; state, code and arbitrary error fields must not escape. No credential-bearing
Location, token request/response body, cookies, Authorization header, calendar IDs, Meet
URLs or raw provider errors in logs. Log only allowlisted operation/status/reason and an
internal correlation ID. Google specifically discourages access tokens in query parameters
because they leak into logs: use `Authorization: Bearer <token>`.[web][overview][best]

Before implementation/live acceptance, record **without secret values**:

1. Operator/client owner, intended one account and applicable personal-use/verification
   category; current External/In production status, consent-screen configured scopes,
   Calendar API enablement, support/contact requirements. These remain unverified.[web][audience]
2. Exact Web client ID reference, private secret provisioning mechanism, exact registered
   callback and browser reachability/TLS plan; stable token-encryption key reference and
   missing-key behavior. No console or secret changes are authorized by this note.
3. Booking Calendar concrete ID and ownership/manager decision; Blocking Calendar choices
   and actual free/busy access under **these exact scopes**. Record no guessed account identity.
4. Parent decisions on unexpected granted scopes, secondary-calendar ownership ambiguity,
   PKCE Web-client verification, strict URI policy, finite HTTP/pagination/poll/cleanup
   limits, and handling original-account cleanup when reconnect changes accounts.
5. Explicit permission/window for **one real test event**: create with `createRequest`
   and version 1, GET/poll success, inspect actual video entryPoint, DELETE, retain evidence
   or unresolved cleanup. No attendees or Patient data. This gate is not routine CI.
6. Synthetic full-router + real-Postgres coverage: exact outgoing scopes/PKCE/state;
   forged/replayed/denied callback; partial/extra grants; no-token reconnect/account-switch
   isolation; encryption/key failures; refresh omission/replacement/concurrency and late
   response fencing; all CalendarList pages/hidden/deleted/new role values; free/busy
   per-item errors; pending/failure/invalid URI; duplicate/uncertain insert; conditional
   delete/204/ambiguous 404/410; restart-retained cleanup and capability invalidation.
   Fixtures prove application behavior, not Google's live policy or real account capability.

[discovery]: https://www.googleapis.com/discovery/v1/apis/calendar/v3/rest
[auth]: https://developers.google.com/workspace/calendar/api/auth
[web]: https://developers.google.com/identity/protocols/oauth2/web-server
[overview]: https://developers.google.com/identity/protocols/oauth2
[native]: https://developers.google.com/identity/protocols/oauth2/native-app
[best]: https://developers.google.com/identity/protocols/oauth2/resources/best-practices
[policies]: https://developers.google.com/identity/protocols/oauth2/policies
[production]: https://developers.google.com/identity/protocols/oauth2/production-readiness/policy-compliance
[oidc-discovery]: https://accounts.google.com/.well-known/openid-configuration
[openid]: https://developers.google.com/identity/openid-connect/openid-connect
[audience]: https://support.google.com/cloud/answer/15549945?hl=en
[exemptions]: https://support.google.com/cloud/answer/13464323?hl=en
[unverified]: https://support.google.com/cloud/answer/7454865?hl=en
[list]: https://developers.google.com/workspace/calendar/api/v3/reference/calendarList/list
[list-resource]: https://developers.google.com/workspace/calendar/api/v3/reference/calendarList
[sharing]: https://developers.google.com/workspace/calendar/api/concepts/sharing
[freebusy]: https://developers.google.com/workspace/calendar/api/v3/reference/freebusy/query
[event]: https://developers.google.com/workspace/calendar/api/v3/reference/events
[insert]: https://developers.google.com/workspace/calendar/api/v3/reference/events/insert
[get]: https://developers.google.com/workspace/calendar/api/v3/reference/events/get
[delete]: https://developers.google.com/workspace/calendar/api/v3/reference/events/delete
[errors]: https://developers.google.com/workspace/calendar/api/guides/errors
[create]: https://developers.google.com/workspace/calendar/api/guides/create-events
[extended]: https://developers.google.com/workspace/calendar/api/guides/extended-properties
[version]: https://developers.google.com/workspace/calendar/api/guides/version-resources
[meet-space]: https://developers.google.com/workspace/meet/api/reference/rest/v2/spaces
