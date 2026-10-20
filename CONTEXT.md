# Medical Assistant

A WhatsApp receptionist and one-screen dashboard for a single independent doctor's practice in Mexico. It handles scheduling and video-payment verification; it is not a clinical record.

Glossary terms are English; patient- and doctor-facing copy is Spanish (the UI label is noted where it is fixed).

## Language

### Practice and people

**Practice**:
The one doctor's office a deployment serves, including its location, timezone, services and payment details.
_Avoid_: tenant, clinic, organization

**Doctor**:
The single person who owns the Practice and the only one who can approve, verify or change records.
_Avoid_: admin, user, staff

**Practice Number**:
The WhatsApp number Contacts write to, operated by the assistant.
_Avoid_: bot number, business number, clinic number

**Doctor Number**:
The Doctor's own WhatsApp number. It receives the Doctor's notifications and can ask about the agenda, but cannot change records.
_Avoid_: admin number, verified number

**Contact**:
A WhatsApp number that messages the Practice; the party the assistant converses with and notifies.
_Avoid_: patient, user, customer

**Patient**:
The person attending an Appointment, recorded by name on that Appointment only. Not a persistent record; one Contact may book for several Patients.
_Avoid_: client; Contact (when meaning the attendee)

### Scheduling

**Service**:
A kind of consultation the Practice offers, with its own duration and price in MXN.
_Avoid_: product, treatment

**Format**:
Whether an Appointment happens in person at the Practice's location or by video. Only video Appointments require payment before they are confirmed.
_Avoid_: modality, type, mode

**Appointment** (UI: *cita*):
The single record of one visit, with a stable identity from first request until it is cancelled, expires or takes place. A reschedule changes the same Appointment rather than creating a new one. Once its time has passed it is simply past; there is no "completed" or "no-show".
_Avoid_: booking, reservation, completed, no-show

**Open Appointment**:
An Appointment that is not cancelled, expired or past. A Contact may have only a few at once.
_Avoid_: active booking

**Expired Appointment**:
A video Appointment whose Hold lapsed because no timely Receipt arrived.
_Avoid_: cancelled (cancellation is always someone's decision)

**Reschedule** (UI: *cambiar cita*):
Moving an Appointment to another time with the same Service and Format, carrying its payment along. Any other change is a Followup Request.
_Avoid_: rebooking, modification

**Appointment History**:
The append-only record of every change to an Appointment and who made it: the Doctor, the Contact or the System.
_Avoid_: log, audit log

**Hold**:
A claim on a time range that blocks it for one Appointment, optionally with an expiry. Whether the Appointment is confirmed belongs to the Appointment, not the Hold. An Appointment normally has one Hold; during a reschedule it has two until the new one replaces the old.
_Avoid_: slot lock, reservation, tentative hold, secured hold

**Booking Calendar**:
The Doctor's Google calendar that receives Appointment events.
_Avoid_: main calendar

**Blocking Calendar**:
A Google calendar, selected by the Doctor, whose events make time unavailable for Appointments. The Booking Calendar is usually one too.
_Avoid_: busy calendar, external calendar

**Reminder** (UI: *recordatorio*):
A message sent to a Contact ahead of a confirmed Appointment, inviting a reply to confirm attendance.
_Avoid_: notification (too broad)

**Followup Request**:
A Contact's request for the Doctor's personal attention, including anything the assistant cannot book within the Practice's rules.
_Avoid_: exception, exception request, escalation, ticket

**Takeover**:
A period in which the assistant stays silent toward one Contact because the Doctor is writing to them directly from the Practice Number's phone.
_Avoid_: pause, handoff, human mode

### Payment

**Receipt** (UI: *comprobante*):
An image or PDF a Contact submits as evidence of a bank transfer for a video Appointment. Evidence only, never proof that funds arrived.
_Avoid_: proof of payment, payment

**Payment Reference**:
A short code unique to one Appointment that the Contact puts in the bank transfer's *concepto*, so the Doctor can match the deposit.
_Avoid_: folio, payment ID

**Receipt Deadline**:
The time by which a Contact must send a Receipt for a video Appointment, judged by WhatsApp's own message time rather than by when the message reached the assistant.
_Avoid_: payment deadline, expiry

**Late Receipt**:
A Receipt that reaches the assistant after its Appointment's Hold has expired, whether or not it was sent before the Receipt Deadline.
_Avoid_: expired receipt

**Payment Verification** (UI: *Confirmar pago recibido*):
The Doctor's explicit statement that the transferred funds actually arrived; the only thing that makes a video Appointment paid. Its opposite is rejecting the Receipt (UI: *No recibí el pago*).
_Avoid_: receipt approval, payment confirmation

### Dashboard

**Attention Item**:
Something on the dashboard that needs the Doctor's decision or action: a Receipt to review, a Late Receipt, a calendar conflict, a failed sync, a lost connection or a Followup Request.
_Avoid_: alert, task, ticket
