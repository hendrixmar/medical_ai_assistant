# WAHA / GOWS messaging contract for issue #7

Research date: **2026-10-14**. Scope: one Practice, session **`default`**, engine
**GOWS**, WAHA **2026.9.2**, commit
[`90ba445a0e765701c8910f0fac08176250c3e0fd`][waha-pin].

This is research, not implementation or provider acceptance. Constraints came from
`CONTEXT.md`, ADR-0001, ADR-0002, the supplied issue #7 snapshot, and
[`waha.md`](waha.md). Issue #6's **79 passing application tests do not complete its
operator-blocked real-provider acceptance**. No live WAHA requests, credentials or
`.env` reads, pairing, messages, configuration/deployment changes, tracker operations,
other agents, code edits, commits or pushes were performed. Only this document was
written in the repository; public source copies are under
`/tmp/medical-waha-messaging-source/`.

**Subsequent local implementation:** [conversation/outbox application record](conversation-outbox.md)
describes the implemented #7 slice and synthetic evidence separately from this research.
No source claim below is live-provider acceptance. The pinned direct serialized-ID parser
requires exactly three underscore-separated parts (four means a participant); the client
therefore rejects underscores inside opaque raw IDs and enforces the 512-byte serialized
bound before sending. Other non-control nonhex characters are retained, not guessed hex.

## 1. Authority and bottom line

The pinned WAHA [`waha.config.json`][build-config] selects
**`devlikeapro/gows-plus`, `v1.0.48`**; [`Dockerfile`][docker] downloads that release's
binary. Public GitHub resolves that tag to
**`120a614810791574c9f682c504997ea7caa6ab11`**. Its [`src/go.mod`][go-mod] replaces
WhatsMeow with the first-party fork at
**`22e2b8a0bdc533fd515f60057c34cae318f8b706`**. These are the engine sources below,
not another engine's behavior. This chain establishes source provenance, not an
attestation of the running binary. Website documentation is rolling guidance
retrieved on the research date, subordinate to pinned implementation details.

**A conservative durable outbox can rely on:**

- A pre-generated/caller-supplied WhatsApp message ID passed through `sendText`.
- A successful GOWS send returning a serialized WAHA message ID after WhatsApp's
  server acknowledgement, **not recipient delivery**.
- Signed `message.ack` events reporting recipient-device/read/played/error outcomes,
  subject to duplicates, loss, reordering and phone/LID aliases.
- Explicit pre-send rejections being distinguishable from some ambiguous failures.

**It cannot rely on:** an HTTP idempotency-key contract, a cached duplicate-submit
result, guaranteed delivery of acknowledgements, cancellation of a send when the
HTTP client times out, or exactly-once delivery after retry/restart. A persisted
logical response and a stable provider ID help correlation; neither makes a second
POST safe. **Unknown send outcomes require an explicit uncertain state, not an
automatic resend or a false terminal “not sent” assertion.** [S1–S6 below]

## 2. HTTP operations and exact GOWS behavior

All paths are relative to the configured WAHA origin, authenticated with the existing
server-side `X-Api-Key` contract. Send JSON and explicitly include `session:"default"`.
The controller checks `Action.Send` and `getWorkingSession` before these four POST
operations. DTO initializers are not a substitute for explicit client fields:
there is **no global transforming validation pipe**, and these routes have no local
validation pipe. Do not assume every malformed DTO becomes a clean 400. [S1, S7]

| Operation | Request fields | GOWS success result |
| --- | --- | --- |
| `POST /api/sendText` | `session`, `chatId`, `text`; optional `id`, `mentions: string[]`, `reply_to`, `linkPreview`, `linkPreviewHighQuality` | Exactly the wrapper `{ "id": "true_<chat>_<WhatsApp-ID>", "_data": <raw message or null> }`; **not** the full advertised `WAMessage` DTO |
| `POST /api/sendSeen` | `session`, `chatId`, preferably `messageIds: [<original serialized incoming payload.id>]`; deprecated `messageId`; optional `participant` (DTO describes this as NOWEB-only) | Explicit-ID GOWS path returns `undefined` / no JSON body. Without either ID field, a different read-history path returns `{ "ids": [...] }` |
| `POST /api/startTyping` | `session`, `chatId` | `{ "result": true }` |
| `POST /api/stopTyping` | `session`, `chatId` | `{ "result": true }` |
| `GET /api/default/new-message-id` | No body; working-session, `Action.Read` boundary | `{ "id": "<raw WhatsApp-ID>" }`; it generates an ID, does not send a message |

The routes do not override Nest's normal response status (normally POST **201**, GET
**200**); those status/body combinations are source-derived, **not wire-verified**
on this Practice. A decoder must not require a JSON body for explicit-ID `sendSeen`,
nor infer success from status alone when a send ID is missing/malformed. [S1–S3]

### Text request and response details

`MessageTextRequest` declares `text` default `"Hi there!"`, `linkPreview` default
`true`, `linkPreviewHighQuality` default `false`; GOWS explicitly applies
`request.linkPreview ?? true`. `id` means a **raw pre-generated message ID**, not the
serialized `true_...` ID. `reply_to` is a quoted-message reference, not idempotency;
GOWS extracts its raw ID. Mentions are resolved through identity hooks. For #7,
explicit text and `linkPreview:false` are a reasonable **application policy** to
avoid incidental preview fetching; no mentions or quoted reply are required. [S2, S3]

GOWS `sendText` builds a gRPC `MessageRequest`, including `id: request.id`, and awaits
`SendMessage`. Go assigns nonempty `req.Id` to `SendRequestExtra.ID`. The underlying
`Client.SendMessage` waits for a server acknowledgement. Go then constructs a raw
`events.Message` with `Info.{Chat,Sender,IsFromMe,IsGroup,ID,Timestamp,ServerID}`,
`Message` and `RawMessage`; WAHA serializes its public response via `messageResponse`.
The public `id` is a **string**; preserve it verbatim. For a direct chat it is
`true_<toCusFormat(request-resolved-JID)>_<returned-raw-ID>`; group IDs have another
participant component, out of scope here. There is **no top-level `ack`, `timestamp`,
`fromMe`, `to`, or `body` in this send response**. `_data` is raw engine information,
not a durable normalized contract or safe log payload. [S3–S5]

### Seen and typing

`ExtractMessageKeysForRead` parses **serialized IDs strictly** (`soft=false`), skips
`fromMe=true`, and prefers the chat embedded in each ID over `chatId`. A bare raw ID
therefore fails on this pinned path. Pass the actual incoming `payload.id`, never
an envelope event ID, a newly generated outgoing ID, or a reconstructed ID using a
guessed alias. Validate that the retained ID belongs to the intended Contact.
The pinned parser splits on **every underscore**, requires exactly three or four
parts, and interprets the fourth as a participant. A raw ID containing an underscore
would therefore be misinterpreted or rejected by this seen path. The application's
supported direct-chat ID contract must exclude that delimiter from raw IDs, while
remaining opaque otherwise (no invented hex-only requirement). Bound the complete
serialized outgoing ID before dispatch, not only the raw component. This source
constraint was confirmed during transport review; malformed inputs stay durable
but must not trigger a guessed or broader seen operation.
`messageIds` is the exact spelling; the rolling send-message page says
`messagesIds` in prose, but source and its example use `messageIds`. [S1–S3, S8]

Original IDs are not universally required by the endpoint: omitting them invokes
stored-message lookup, defaulting to seven days and returning the IDs marked read.
That can read more than the triggering message and depends on provider storage.
For #7, using the durable original incoming ID is the narrower, deterministic
**application policy**. Both `@c.us` and `@lid` survive the relevant conversion;
phone JIDs become `@s.whatsapp.net` internally. GOWS aggregates keys and calls
`MarkRead`; the lower layer supports phone and LID direct chats, with no group
participant required. Read-receipt privacy settings can make it `read-self`, so
endpoint completion is not proof the Contact saw blue ticks. [S8, S9]

Typing maps to GOWS `setPresence(TYPING)` / `setPresence(PAUSED)`, then gRPC
`SendChatPresence`. Neither sends text nor confirms text delivery. The controller
calls starting typing an indefinite action: stopping it is a separate operation.
A failure in seen/typing must not be confused with a failure of a text send that
already happened. [S1, S3]

## 3. IDs do not provide a proven idempotent resend

**Supported:** caller-supplied `id`, plus the provider's generator endpoint. Go
passes that ID to WhatsMeow; WhatsMeow generates one only when none was supplied.
Persisting a generated ID before attempting text allows acknowledgements to be
correlated even if the HTTP response is lost. Generating another unused ID after a
lost *generator* response does not send a duplicate message. [S1–S5]

**Not established:** any `Idempotency-Key` header, duplicate POST suppression,
request-body hash check, durable cached HTTP result, or guarantee that submitting
the same ID twice delivers only once. The inspected `SendMessage` path calls the
engine again on every request. WhatsMeow's recent-message/retry buffer stores content
for protocol retry receipts; `addRecentMessage` does not return a previously
completed send result. WhatsMeow can also internally retransmit the **same frame**
after reconnect. That narrowly scoped protocol mechanism is not a contract for
reissuing a whole HTTP request, especially across process restarts. [S3–S6]

### Outcome classification: what is actually known

Classification applies to **one attempt**. A later rejection cannot prove that an
earlier uncertain attempt did not send.

| Observation | What it establishes | Conservative application handling |
| --- | --- | --- |
| Failure demonstrably before any HTTP request could reach WAHA (for example DNS/connect failure before request transmission, with no hidden client/proxy retry) | This attempt did not invoke send | Eligible for bounded retry after rechecking durable incoming evidence and policy |
| Authentic WAHA auth/policy rejection before controller invocation; exact working-session rejection below | This request never reached `sendText` | Failed/blocked or retryable only after prerequisite recovery; never repair pairing/config automatically |
| Complete successful `sendText` response with valid expected ID | WhatsApp server acknowledged submission | Record **sent**, not delivered/read; never repeat just because no ack arrives |
| Recognized underlying `server returned error 463` / `475` negative acknowledgement | Provider rejected this attempt for reachout timelock / new-chat quota | Record rejection and pause; no blind backoff loop, restart, logout or re-pair |
| Other error proven to originate before message transmission | This attempt did not send | Narrow allowlist only, grounded in the exact source branch; error text/status alone is insufficient |
| Client timeout, reset, cancellation, truncated/invalid success body, response-size failure, unexplained 5xx/502/503/504, crash during call, or success received but lost before application commit | May have sent; response/commit may be the only lost part | **Uncertain**, preserve attempt/ID, reconcile later positive evidence or require explicit operator decision; no automatic text resend |
| `message.ack` with `ERROR=-1` | Provider reported an error for that message | Preserve failure evidence; not proof of zero prior delivery, and not permission to resend |
| No ack, or later history lookup returns no message | Nothing conclusive about delivery/non-send | Stay at last justified state; absence is not rejection |

**Known error shapes and traps.** `getWorkingSession` can emit HTTP **422**, before
calling the engine, with these exact object shapes: [S7]

```json
{"error":"Session name is required","session":""}
{"error":"Session \"default\" does not exist","session":"default"}
{"error":"Session status is not as expected. Try again later or restart the session","session":"default","status":"STOPPED","expected":["WORKING"]}
```

`status` can be another actual non-WORKING state. “Restart” in provider wording is
**not authorization** for the application to restart anything. Note that missing
session here is **422**, not the session-status GET endpoint's 404. The policy guard
throws `ForbiddenException` before the handler. Do not generalize an arbitrary HTTP
4xx from a gateway into one of these proven branches. [S1, S7]

The global exception filter passes `HttpException` responses through. Other errors
become HTTP **500** with fields
`{statusCode:500,timestamp,exception,request:{path,method,body,query},version}`.
`exception` contains serialized own error properties, potentially `message`,
`details`, `code`, `name`, `stack`; this is **not a fixed safe public error schema**.
It can echo the complete text/request body: never log or surface it verbatim. [S10]

The pinned underlying error strings include
`"timed out waiting for message send response"`, `"websocket not connected"`,
`"the store doesn't contain a device JID"`, and `"server returned error <number>"`.
For direct text, not-logged-in is checked before sending. Conversely,
`websocket not connected` can also come from the reconnect path **after the first
frame was sent**. A 500 containing it must not automatically retry. WhatsApp error
**463/475 are not HTTP status codes**: plain Go errors propagate through gRPC and
normally reach WAHA's generic HTTP-500 filter. Validate the actual serialized gRPC
error contract before making a narrow machine-readable rejection classifier;
unknown codes/errors remain uncertain. [S4–S6, S10]

**Timeout mismatch matters:** WAHA can first wait 5 seconds for a running session
and 10 seconds for WORKING. WhatsMeow's normal send-response timeout is 75 seconds,
after preparation; its reconnect-frame path here is called with timeout zero and
can wait for context cancellation. WAHA's TypeScript gRPC invocation supplies no
explicit deadline or HTTP-abort cancellation binding. The existing application's
five-second status/QR client deadline must **not** be interpreted as “the message
was not sent.” A larger chosen send deadline still cannot eliminate ambiguity.
[S3, S5–S7]

## 4. `message.ack`: subscription, authentication and correlation

### Subscription and signed envelope

A future **authorized operator** must add `"message.ack"` alongside `"message"` in
the existing `config.webhooks[].events`, preserving all other configuration and
`hmac.key`. `message.ack` is the direct-contact event; `message.ack.group` is separate
and not needed. Registering only `message` does not subscribe to acknowledgements.
No configuration was changed in this research. [S11]

The envelope shape is:

```text
{
  id: "evt_<lowercase-ULID>", timestamp: <event milliseconds>,
  event: "message.ack", session: "default", engine: "GOWS",
  metadata?: <session metadata>, me: <Practice account info>,
  environment: <WAHA version/environment object>,
  payload: {
    id: "true_<peer-chat>_<raw-WhatsApp-ID>",
    from: "<peer-chat>", to: null, participant: null,
    fromMe: true, ack: 2, ackName: "DEVICE", _data: <raw receipt>
  }
}
```

This is a **source-derived schematic of a direct outgoing delivery receipt**, not a
captured live event. Some metadata is optional; other ack values/directions exist.
`payload` has **no normalized timestamp or `sender` field**. In GOWS direct-chat
acks, `from` is the receipt's **chat/peer** and `to`/`participant` are null; do not
apply another engine's “from is always the Practice sender, to is the Contact”
assumption. `fromMe` describes the **referenced message**, computed as
`!receipt.IsFromMe`. Marking an incoming message seen can itself yield an ack with
`fromMe:false`: it must not update an outgoing message. [S12, S13]

`_data` retains the receipt, with `Chat`, `Sender`, `IsFromMe`, `IsGroup`,
`MessageIDs`, `Timestamp`, `Type`, `MessageSender` and optional/empty alternate
address fields such as `SenderAlt`/`RecipientAlt`. These are receipt fields at
`_data.<field>`, **not `_data.Info.<field>`**. `Sender` is the receipt actor;
`MessageSender` is the original-message sender when supplied. GOWS emits a separate
normalized ack for every ID in `MessageIDs`. [S13, S14]

Webhook signing is unchanged: HMAC-SHA512 over exact `JSON.stringify` UTF-8 bytes,
`X-Webhook-Hmac` lowercase hex, `X-Webhook-Hmac-Algorithm: sha512`; request-ID and
timestamp headers are not message IDs. Authenticate raw bytes before interpreting
any ack; require the configured session/engine, valid direction and known outgoing
correlation. Unknown/mismatched events must not create Contacts, authorize replies,
or confer Doctor privileges. [S11–S13]

### Ack meanings and actual GOWS event production

| `ack` / `ackName` | Meaning | Pinned GOWS `message.ack` behavior |
| --- | --- | --- |
| `-1` / `ERROR` | Reported message error | Receipt `Type:"server-error"` |
| `0` / `PENDING` | Provider-pending/queued, not application queue durability | Defined in enum, **not emitted** by this receipt converter |
| `1` / `SERVER` | WhatsApp server acknowledged | Defined in enum; **not emitted** by this receipt converter; successful send is server-ack evidence, and outgoing `message.any` is assigned SERVER |
| `2` / `DEVICE` | Delivered to recipient device, not attendance or human attention | Receipt types `""`, `"inactive"`, `"active"` |
| `3` / `READ` | Reported read | Receipt `"read"` |
| `4` / `PLAYED` | Reported played/viewed media | Receipt `"played"`; not a guarantee of voice comprehension; dependency also documents view-once-media viewing |

Other receipt types are dropped by this converter, including `read-self`,
`played-self`, `sender` and retry receipts. Do not require a full 0→1→2→3 sequence,
or infer a failure when read receipts never arrive. Provider PENDING is distinct
from a transactionally queued application outbox entry. Neither send success nor
any delivery/read/played acknowledgement confirms Appointment attendance. [S13–S15]

### IDs, ordering and alias changes

The send response and ack both use GOWS `buildMessageId`; with the same peer
representation their serialized IDs match exactly. But phone-targeted sends can be
**converted internally to LID** by WhatsMeow, while the WAHA send response retains
the input-resolved chat. A later receipt may therefore have
`true_<lid>@lid_<same-raw-ID>` rather than
`true_<phone>@c.us_<same-raw-ID>`. Preserve both the raw WhatsApp ID and exact returned
ID; correlate only for the same session/outgoing attempt and a peer backed by
trusted durable alias evidence. Do not blindly match unrelated chats by suffix,
phone-looking LID digits, body, or timing. Unresolved/conflicting aliases mean
unmatched evidence needing reconciliation, not permission to resend. [S3, S5, S13, S16]

Acks can arrive **before the application's HTTP response or its commit**: receipt
events flow independently; Go launches outgoing events asynchronously; webhook
sender requests are independently scheduled and not serialized behind the API
response. There is no cross-channel ordering barrier. Preserve authenticated early
or unmatched acks rather than discarding them because an outbox result is not yet
recorded. Do not downgrade READ to DEVICE when a stale ack arrives; preserve
conflicting error evidence rather than silently erasing a stronger prior positive
outcome. These are **application policies**, not provider guarantees. [S4, S11, S13]

GOWS `DistinctAck` deduplicates `(id,ack,participant)` only in memory, flushing every
**60 seconds**. Webhook sender retries every error, default **15 retries**, constant
**two seconds**, with independent requests; restart can lose pending deliveries.
The Go event stream also drops events when a listener's **1000-event** buffer fills.
Receipt `MessageIDs` order varies between Android and iOS. Therefore durable
application deduplication is necessary, and neither ordering nor eventual ack
arrival is guaranteed. Event-envelope ID deduplication alone does not suppress a
new envelope reporting the same message/ack again. [S11, S14, S16, S17]

## 5. Identity, targeting and durable incoming evidence

**Explicit direct `@lid` is supported in source.** WAHA's `toJID` passes it through;
the GOWS identity plugin normalizes it; WhatsMeow handles `HiddenUserServer` in its
direct-message send branch. `@c.us` is also supported, converted internally to
`@s.whatsapp.net` and potentially to a known/resolved LID. Do not pass a fabricated
phone from LID digits; lack of phone resolution is not itself evidence that direct
LID sending is unsupported. Actual account/device delivery remains a live gate.
[S3, S5, S9]

For incoming GOWS messages, the official helper resolves phone identity from
`_data.Info.SenderAlt`; outgoing messages use `RecipientAlt`. Persist the original
identity and only verified alternate identities. Use the original incoming chat as
the conservative reply target unless a durable trusted mapping justifies another
representation. Neither a provider number lookup, an ack, a successful send, nor
an identity alias establishes that a Contact **wrote first**. Seen, typing and text
must be restricted to a chat whose incoming evidence was already durably committed.
Aliases do not authorize Doctor actions. [S18; issue #7 and existing intake contract]

## 6. Reliable unsupported-input identification

Keep the existing intake boundary: authenticated `event=message`, `session=default`,
`engine=GOWS`, `payload.fromMe=false`, and direct numeric `@c.us`/`@lid` identities.
Outgoing messages, calls, groups and other event types do not enter the ordinary
Contact-message conversation path. A separate ack handler must not weaken that rule.

The normalized GOWS `WAMessage` exposes `hasMedia`, optional `media.{url,mimetype,
filename,...}`, body and `_data`; **it has no normalized `type`, `isVoice`, `isPtt`,
`isSticker`, or `isAudio` field**. `hasMedia` cannot distinguish audio from images,
documents, video or stickers. `media` may be null when downloading fails/is disabled;
file extension and `audio/ogg` MIME alone cannot distinguish voice from audio files.
No media download or transcription is needed for #7. [S19]

The public dependency source supplies exact, case-sensitive raw fields. GOWS uses
Go `encoding/json`, so the protobuf JSON tags matter: **`PTT` is uppercase**, not
`ptt` in the raw webhook. The WAHA helper that camel-cases a separate proto view
does not rewrite `_data`. [S17, S20]

| Kind | Available identification | #7 consequence / limitation |
| --- | --- | --- |
| Voice note | `_data.Message.audioMessage` with boolean **`PTT:true`** | Reply `No puedo escuchar audios; por favor escríbame` plus menu; do not claim to listen |
| Other audio message | `_data.Message.audioMessage` with `PTT:false` or absent | Audio known; no affirmative PTT evidence. Same text+menu response is the requested application policy |
| Audio uploaded as a file | `_data.Message.documentMessage`, with its `mimetype` beginning `audio/`, or downloaded `media.mimetype` | File category, not necessarily voice; audio fallback is application policy, not a provider subtype |
| Other file/document | `_data.Message.documentMessage`; `mimetype`/`fileName` in raw protobuf | Do not confuse with successful Receipt handling; that journey is future work |
| Image/video/video-note | `_data.Message.imageMessage`, `videoMessage`, `ptvMessage` | Media, not voice; `hasMedia` alone is insufficient |
| Sticker | `_data.Message.stickerMessage`; `_data.IsLottieSticker:true` / `lottieStickerMessage` wrapper where present | Ignore; do not classify solely by `image/webp`, which could be an ordinary file |
| Audio sticker | Exact protobuf field `audioStickerMessage` exists | Treat an explicitly recognized sticker wrapper as sticker, not a voice note. Its real GOWS normalization/delivery shape is **unverified**; do not invent its inner layout |
| Reaction | Event `message.reaction`, normalized `reaction.{text,messageId}`; raw `_data.Message.reactionMessage` when present | Ignore. GOWS filters normalized reaction content out of ordinary `message`; empty reaction text means removal |
| Status/group | `status@broadcast`, group `@g.us`, raw `Info.IsGroup`, existing direct-chat allowlist | Ignore status updates and groups; a direct Contact message quoting a status is a separate case, not automatically a status update |
| Call | Event `call.received` (also accepted/rejected), payload `{id,from,timestamp,isVideo,isGroup,_data}` | **Reply remains DISABLED** without authorized actual Practice incoming-call delivery evidence, and still subject to durable wrote-first evidence |

The Go `events.Message` exposes an unwrapped **`Message`** and original
**`RawMessage`**. `UnwrapRaw` handles device-sent, bot-invoke, ephemeral, view-once
variants, Lottie sticker, document-with-caption and edited wrappers. Prefer the
actual supported `Message` path; do not search quoted-message media and mistake it
for the new message. Missing/unknown/conflicting raw structures stay unknown; never
guess fields or classify any nonempty body/caption as menu input before excluding
sticker/audio/media. Raw fields are explicitly engine/version-sensitive. [S13, S19–S21]

## 7. Pacing: exact recommendation versus application policy

The official [“How to avoid blocking” page][pacing] recommends this sequence:

1. **“Send seen before processing the message.”**
2. **“Start typing before sending a message and wait for a random interval depending
   on the size of the message.”**
3. **“Stop typing before sending the message.”**
4. Send text with `POST /api/sendText`.

It also says **only reply** rather than initiate, avoid unnecessary messages, and
send one short opening message. Its broader dos/don'ts say to wait a random
**30–60 seconds between first and second messages** and give this **example**, not
an API-enforced universal quota: **maximum four messages per replied Contact over
one hour, then stop sending for one hour before another batch**. These are the
first-party words, not independently validated anti-ban or compliance guarantees.
No exact typing-length formula or minimum/maximum typing duration is supplied.

**User-approved application policy for #7:** interactive replies, with a
size-dependent random typing delay bounded to **2–6 seconds**, at most **one
automatic reply per incoming message**, and an operator-configurable default cap
of **20 replies per Contact per rolling hour**. The user chose this instead of a
literal adoption of the 30–60-second/four-per-hour examples. These numbers are
application policy, **not a WAHA-endorsed safe threshold or protection against
blocking**. No separate Practice-wide quota was approved. Doctor-triggered
resolution notices are separate logical responses and must respect the same
Contact sending budget.

The subsequent [local implementation](conversation-outbox.md) supplies persisted sampled
delays, rolling-window eligibility, pre-send retry budgets, lease/fence handling and
truthful deferred/failed/uncertain outcomes, verified with controlled-clock synthetic
provider/real-Postgres tests. These local results do not establish provider safety. Keep
factual Spanish templates deterministic—do not adopt the page's unrelated
suggestions to randomize wording/spaces or use URL shorteners. ADR-0002 still governs.

**Different concept: WhatsApp account capping.** Pinned `MeInfo.messageCapping`
exposes `cappingStatus` (`NONE`, `FIRST_WARNING`, `SECOND_WARNING`, `CAPPED`, an open
set), `totalQuota` (`-1` means no cap), `usedQuota`, `cycleStart`/`cycleEnd` (seconds
or null), `mvStatus`, `oteStatus`. It is a provider new-chat quota, not the
application's per-Contact reply limit; a DTO example of 1000 is not a chosen cap.
Source detects negative acknowledgements **475** (capped) and **463** (reachout
timelock), refreshing metadata while the session can remain WORKING. No restart,
logout or re-pair resets these restrictions. [S4, S22, pacing]

## 8. Explicit unknowns and acceptance gates

Before provider-dependent #7 acceptance, retain the unresolved #6 operator gates
in [`waha.md`](waha.md): authorized account/pairing, Postgres session-storage cutover
and provider recreation proof, secure callback provisioning, and real incoming
message delivery. This research neither closes nor bypasses them.

Additional narrowly scoped, **future authorized** evidence is needed for:

- Actual pinned binary/version provenance and status/body/error serialization for
  the four messaging endpoints and generator, without exposing content/secrets.
- Controlled phone and direct-LID replies only to Contacts with durable incoming
  evidence; same raw ID across send result and signed DEVICE/READ acks, including
  alias changes and early acknowledgements.
- Caller-supplied ID behavior on the account. Repeating an ID experimentally could
  duplicate a real message; **not authorized here**, and even one deduplication
  observation would not establish a universal exactly-once guarantee.
- Real raw voice, ordinary-audio, audio-file and sticker payloads, including wrappers
  and unavailable media. Source-backed synthetic fixtures are not live proof.
- Operator-selected pacing/caps and launch approval; no source-derived ban immunity.
- **Calls remain disabled** until separately authorized incoming-call evidence
  establishes actual Practice delivery. Source event support alone is insufficient.

Synthetic application tests can independently enforce: exact ID and DTO contracts;
empty seen response; signed ack validation/replay; DEVICE/READ out of order; ack
before HTTP completion; phone/LID mismatch without guessed merge; ignored
`fromMe:false` ack; duplicate logical enqueue; pacing across restart; definitive
pre-send rejection; provider timeout/reset/500 and crash-after-send remaining
**uncertain without a second POST**. A later positive correlated ack may resolve
uncertainty; missing acks/history must not. These are acceptance implications,
not implementation or a replacement architecture.

## Primary-source index

Every GitHub link below is pinned. Labels used above group closely related exact
functions; line anchors point at the inspected versions.

- **S1 — HTTP routes:** [`ChattingController.sendText`][controller-text],
  [`sendSeen`, `startTyping`, `stopTyping`][controller-seen],
  [`getNewMessageId`][controller-id].
- **S2 — DTOs:** [`ChatRequest`, `SendSeenRequest`][dto-seen],
  [`MessageTextRequest`][dto-text], [`GeneratedMessageIdProperty`][dto-id].
- **S3 — WAHA GOWS:** [`generateNewMessageId`, `sendText`][gows-send],
  [`messageResponse`][gows-response], [`sendSeen`/typing][gows-seen],
  [`setPresence`][gows-presence], [`buildMessageId`][gows-id].
- **S4 — Go service:** [`Server.SendMessage`, `extra.ID`][go-send-id],
  [text construction][go-text], [response construction][go-response],
  [`GoWS.SendMessage`, 463/475, async event][go-send].
- **S5 — WhatsMeow:** [`SendRequestExtra`, `Client.SendMessage`][wm-send],
  [phone→LID targeting][wm-lid], [send/ack/error sequence][wm-send-result].
- **S6 — Retry boundary:** [`retryFrame`][wm-retry-frame],
  [`addRecentMessage`][wm-recent], [exact errors][wm-errors].
- **S7 — Preflight/validation:** [working-session checks][working],
  [wait constants][working-time], [no global pipe][main], [policy guard][policy].
- **S8 — Seen IDs:** [`ExtractMessageKeysForRead`][read-keys],
  [`parseMessageIdSerialized`][parse-id], [history fallback][read-history],
  [rolling send-message docs][send-docs].
- **S9 — JIDs and read receipts:** [`toJID`, `toCusFormat`][jids],
  [identity hook][wid], [`Server.MarkRead`][go-read], [`Client.MarkRead`][wm-read].
- **S10 — HTTP errors:** [`AllExceptionsFilter`][errors].
- **S11 — Webhooks:** [configuration DTO][hook-config], [subscription plugin][hook-plugin],
  [sender/signing/retries][hook-sender].
- **S12 — Envelope:** [`WAHAWebhook`, `WAMessageAckBody`][hook-dto],
  [`populateSessionInfo`][envelope].
- **S13 — GOWS events:** [subscriptions and directions][gows-events],
  [`receiptToMessageAck`, reactions][gows-acks],
  [`getFromToParticipant`, `buildMessageId`][gows-id],
  [input filtering][gows-filter], [calls][gows-calls].
- **S14 — Raw receipt:** [receipt parsing][wm-receipt],
  [`events.Receipt`, platform ordering][wm-event-receipt],
  [`MessageSource`, aliases][wm-source].
- **S15 — Ack enum/semantics:** [WAHA enum][ack-enum],
  [GOWS status converter][ack-convert], [receipt meanings][receipt-types].
- **S16 — Deduplication:** [`DistinctAck`, `DistinctMessages`][distinct].
- **S17 — Go event JSON and losses:** [`safeMarshal`, `StreamEvents`, buffer][go-events].
- **S18 — First-party identity helper:** [`GOWSHelper.PhoneNumber`][identity].
- **S19 — Normalized/raw media:** [`WAMessage`][message-dto], [`WAMedia`][media-dto],
  [`toWAMessage`, failed media download][gows-message],
  [`extractMediaContent`][media-extract].
- **S20 — Exact raw fields:** [`AudioMessage.PTT` JSON tag][audio-proto],
  [message subtype JSON tags][message-proto], [WAHA camel-case helper][proto-convert].
- **S21 — Unwrapping:** [`events.Message`, `UnwrapRaw`][wm-unwrapped].
- **S22 — Provider quota:** [`MessageCappingData` and `MeInfo`][capping].

[waha-pin]: https://github.com/devlikeapro/waha/tree/90ba445a0e765701c8910f0fac08176250c3e0fd
[build-config]: https://github.com/devlikeapro/waha/blob/90ba445a0e765701c8910f0fac08176250c3e0fd/waha.config.json#L1-L6
[docker]: https://github.com/devlikeapro/waha/blob/90ba445a0e765701c8910f0fac08176250c3e0fd/Dockerfile#L86-L96
[go-mod]: https://github.com/devlikeapro/gows-plus/blob/120a614810791574c9f682c504997ea7caa6ab11/src/go.mod#L65
[controller-text]: https://github.com/devlikeapro/waha/blob/90ba445a0e765701c8910f0fac08176250c3e0fd/src/api/chatting.controller.ts#L77-L87
[controller-seen]: https://github.com/devlikeapro/waha/blob/90ba445a0e765701c8910f0fac08176250c3e0fd/src/api/chatting.controller.ts#L243-L274
[controller-id]: https://github.com/devlikeapro/waha/blob/90ba445a0e765701c8910f0fac08176250c3e0fd/src/api/chatting.controller.ts#L404-L414
[dto-seen]: https://github.com/devlikeapro/waha/blob/90ba445a0e765701c8910f0fac08176250c3e0fd/src/structures/chatting.dto.ts#L85-L112
[dto-text]: https://github.com/devlikeapro/waha/blob/90ba445a0e765701c8910f0fac08176250c3e0fd/src/structures/chatting.dto.ts#L183-L199
[dto-id]: https://github.com/devlikeapro/waha/blob/90ba445a0e765701c8910f0fac08176250c3e0fd/src/structures/properties.dto.ts#L42-L50
[gows-send]: https://github.com/devlikeapro/waha/blob/90ba445a0e765701c8910f0fac08176250c3e0fd/src/core/engines/gows/session.gows.core.ts#L1078-L1121
[gows-response]: https://github.com/devlikeapro/waha/blob/90ba445a0e765701c8910f0fac08176250c3e0fd/src/core/engines/gows/session.gows.core.ts#L1303-L1316
[gows-seen]: https://github.com/devlikeapro/waha/blob/90ba445a0e765701c8910f0fac08176250c3e0fd/src/core/engines/gows/session.gows.core.ts#L1634-L1665
[gows-presence]: https://github.com/devlikeapro/waha/blob/90ba445a0e765701c8910f0fac08176250c3e0fd/src/core/engines/gows/session.gows.core.ts#L2111-L2166
[gows-id]: https://github.com/devlikeapro/waha/blob/90ba445a0e765701c8910f0fac08176250c3e0fd/src/core/engines/gows/session.gows.core.ts#L3487-L3514
[go-send-id]: https://github.com/devlikeapro/gows-plus/blob/120a614810791574c9f682c504997ea7caa6ab11/src/server/messages.go#L47-L105
[go-text]: https://github.com/devlikeapro/gows-plus/blob/120a614810791574c9f682c504997ea7caa6ab11/src/server/messages.go#L295-L317
[go-response]: https://github.com/devlikeapro/gows-plus/blob/120a614810791574c9f682c504997ea7caa6ab11/src/server/messages.go#L574-L588
[go-send]: https://github.com/devlikeapro/gows-plus/blob/120a614810791574c9f682c504997ea7caa6ab11/src/gows/gows.go#L468-L515
[wm-send]: https://github.com/devlikeapro/whatsmeow/blob/22e2b8a0bdc533fd515f60057c34cae318f8b706/send.go#L137-L231
[wm-lid]: https://github.com/devlikeapro/whatsmeow/blob/22e2b8a0bdc533fd515f60057c34cae318f8b706/send.go#L332-L365
[wm-send-result]: https://github.com/devlikeapro/whatsmeow/blob/22e2b8a0bdc533fd515f60057c34cae318f8b706/send.go#L414-L486
[wm-retry-frame]: https://github.com/devlikeapro/whatsmeow/blob/22e2b8a0bdc533fd515f60057c34cae318f8b706/request.go#L150-L235
[wm-recent]: https://github.com/devlikeapro/whatsmeow/blob/22e2b8a0bdc533fd515f60057c34cae318f8b706/retry.go#L37-L95
[wm-errors]: https://github.com/devlikeapro/whatsmeow/blob/22e2b8a0bdc533fd515f60057c34cae318f8b706/errors.go#L17-L25
[working]: https://github.com/devlikeapro/waha/blob/90ba445a0e765701c8910f0fac08176250c3e0fd/src/core/abc/manager.abc.ts#L161-L218
[working-time]: https://github.com/devlikeapro/waha/blob/90ba445a0e765701c8910f0fac08176250c3e0fd/src/core/abc/manager.abc.ts#L55-L58
[main]: https://github.com/devlikeapro/waha/blob/90ba445a0e765701c8910f0fac08176250c3e0fd/src/main.ts#L87-L96
[policy]: https://github.com/devlikeapro/waha/blob/90ba445a0e765701c8910f0fac08176250c3e0fd/src/core/auth/policies.guard.ts#L22-L42
[read-keys]: https://github.com/devlikeapro/waha/blob/90ba445a0e765701c8910f0fac08176250c3e0fd/src/core/utils/convertors.ts#L12-L42
[parse-id]: https://github.com/devlikeapro/waha/blob/90ba445a0e765701c8910f0fac08176250c3e0fd/src/core/utils/ids.ts#L10-L35
[read-history]: https://github.com/devlikeapro/waha/blob/90ba445a0e765701c8910f0fac08176250c3e0fd/src/core/abc/session.abc.ts#L763-L783
[send-docs]: https://waha.devlike.pro/docs/how-to/send-messages/#send-seen
[jids]: https://github.com/devlikeapro/waha/blob/90ba445a0e765701c8910f0fac08176250c3e0fd/src/core/utils/jids.ts#L60-L80
[wid]: https://github.com/devlikeapro/waha/blob/90ba445a0e765701c8910f0fac08176250c3e0fd/src/modules/waha-wid-jid/WidToJIDPlugin.ts#L10-L16
[go-read]: https://github.com/devlikeapro/gows-plus/blob/120a614810791574c9f682c504997ea7caa6ab11/src/server/messages.go#L653-L693
[wm-read]: https://github.com/devlikeapro/whatsmeow/blob/22e2b8a0bdc533fd515f60057c34cae318f8b706/receipt.go#L185-L237
[errors]: https://github.com/devlikeapro/waha/blob/90ba445a0e765701c8910f0fac08176250c3e0fd/src/nestjs/AllExceptionsFilter.ts#L55-L79
[hook-config]: https://github.com/devlikeapro/waha/blob/90ba445a0e765701c8910f0fac08176250c3e0fd/src/structures/webhooks.config.dto.ts#L75-L118
[hook-plugin]: https://github.com/devlikeapro/waha/blob/90ba445a0e765701c8910f0fac08176250c3e0fd/src/modules/waha-webhook/WebhookPlugin.ts#L28-L65
[hook-sender]: https://github.com/devlikeapro/waha/blob/90ba445a0e765701c8910f0fac08176250c3e0fd/src/modules/waha-webhook/WebhookPlugin.sender.ts#L20-L179
[hook-dto]: https://github.com/devlikeapro/waha/blob/90ba445a0e765701c8910f0fac08176250c3e0fd/src/structures/webhooks.dto.ts#L20-L39
[envelope]: https://github.com/devlikeapro/waha/blob/90ba445a0e765701c8910f0fac08176250c3e0fd/src/core/abc/manager.abc.ts#L246-L268
[gows-events]: https://github.com/devlikeapro/waha/blob/90ba445a0e765701c8910f0fac08176250c3e0fd/src/core/engines/gows/session.gows.core.ts#L585-L723
[gows-acks]: https://github.com/devlikeapro/waha/blob/90ba445a0e765701c8910f0fac08176250c3e0fd/src/core/engines/gows/session.gows.core.ts#L3250-L3326
[gows-filter]: https://github.com/devlikeapro/waha/blob/90ba445a0e765701c8910f0fac08176250c3e0fd/src/core/engines/gows/session.gows.core.ts#L2950-L2975
[gows-calls]: https://github.com/devlikeapro/waha/blob/90ba445a0e765701c8910f0fac08176250c3e0fd/src/core/engines/gows/session.gows.core.ts#L3104-L3177
[wm-receipt]: https://github.com/devlikeapro/whatsmeow/blob/22e2b8a0bdc533fd515f60057c34cae318f8b706/receipt.go#L65-L101
[wm-event-receipt]: https://github.com/devlikeapro/whatsmeow/blob/22e2b8a0bdc533fd515f60057c34cae318f8b706/types/events/events.go#L440-L452
[wm-source]: https://github.com/devlikeapro/whatsmeow/blob/22e2b8a0bdc533fd515f60057c34cae318f8b706/types/message.go#L26-L42
[ack-enum]: https://github.com/devlikeapro/waha/blob/90ba445a0e765701c8910f0fac08176250c3e0fd/src/structures/enums.dto.ts#L70-L88
[ack-convert]: https://github.com/devlikeapro/waha/blob/90ba445a0e765701c8910f0fac08176250c3e0fd/src/core/engines/gows/helpers.ts#L25-L41
[receipt-types]: https://github.com/devlikeapro/whatsmeow/blob/22e2b8a0bdc533fd515f60057c34cae318f8b706/types/presence.go#L34-L64
[distinct]: https://github.com/devlikeapro/waha/blob/90ba445a0e765701c8910f0fac08176250c3e0fd/src/core/utils/reactive.ts#L6-L50
[go-events]: https://github.com/devlikeapro/gows-plus/blob/120a614810791574c9f682c504997ea7caa6ab11/src/server/events.go#L16-L138
[identity]: https://github.com/devlikeapro/waha/blob/90ba445a0e765701c8910f0fac08176250c3e0fd/src/apps/chatwoot/waha/engines.ts#L119-L138
[message-dto]: https://github.com/devlikeapro/waha/blob/90ba445a0e765701c8910f0fac08176250c3e0fd/src/structures/responses.dto.ts#L69-L128
[media-dto]: https://github.com/devlikeapro/waha/blob/90ba445a0e765701c8910f0fac08176250c3e0fd/src/structures/media.dto.ts#L4-L38
[gows-message]: https://github.com/devlikeapro/waha/blob/90ba445a0e765701c8910f0fac08176250c3e0fd/src/core/engines/gows/session.gows.core.ts#L2999-L3067
[media-extract]: https://github.com/devlikeapro/waha/blob/90ba445a0e765701c8910f0fac08176250c3e0fd/src/core/engines/noweb/utils.ts#L24-L50
[audio-proto]: https://github.com/devlikeapro/whatsmeow/blob/22e2b8a0bdc533fd515f60057c34cae318f8b706/proto/waE2E/WAWebProtobufsE2E.pb.go#L16346-L16365
[message-proto]: https://github.com/devlikeapro/whatsmeow/blob/22e2b8a0bdc533fd515f60057c34cae318f8b706/proto/waE2E/WAWebProtobufsE2E.pb.go#L10869-L10983
[proto-convert]: https://github.com/devlikeapro/waha/blob/90ba445a0e765701c8910f0fac08176250c3e0fd/src/core/engines/gows/waproto.ts#L4-L32
[wm-unwrapped]: https://github.com/devlikeapro/whatsmeow/blob/22e2b8a0bdc533fd515f60057c34cae318f8b706/types/events/events.go#L322-L424
[capping]: https://github.com/devlikeapro/waha/blob/90ba445a0e765701c8910f0fac08176250c3e0fd/src/structures/sessions.dto.ts#L404-L503
[pacing]: https://waha.devlike.pro/docs/overview/how-to-avoid-blocking/
