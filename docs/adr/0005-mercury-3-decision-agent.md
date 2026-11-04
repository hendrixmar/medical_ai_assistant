# Use Mercury 3 and a single-call `decidir` tool for candidate conversation decisions

Under [ADR 0002](0002-llm-parses-app-states-facts.md), the language model never writes factual replies or executes business mutations directly. Following the Solis MVP agent pattern, the application adds **Mercury 3** (`mercury-3` via Inception's OpenAI-compatible `https://api.inceptionlabs.ai/v1/chat/completions` endpoint with `reasoning_effort="medium"`) and a single-call structured decision tool (`decidir`).

## Decision and execution boundary

1. **Sanitized context only:** Before any model request leaves the process, the application strips phone numbers, six-digit verification codes, 18-digit CLABE numbers, and clinical/untrusted tokens. The model receives only the current conversation stage, allowed option labels, `today` in the Practice timezone, and the sanitized message text.
2. **Outside database locks:** Model calls run during the read-only `prepare` preflight before opening the state-mutating PostgreSQL transaction.
3. **Single `decidir` tool call:** Packaged instructions (`src/modules/conversation/decision.md`) require the model to call `decidir` once with a closed `accion` (`schedule_in_person`, `agendar`, `cambiar_cita`, `cancelar_cita`, `comprobante`, `followup`, `confirmar_asistencia`, `no_asistire`, `elegir_servicio`, `elegir_formato`, or `agenda`), optional exact substrings `date_phrase` and `time_phrase`, optional `formato` or 1-based `opcion`, and `uncertain`. Zero tool calls, multiple tool calls, unknown fields, `uncertain=true`, or non-substring date/time phrases are rejected.
4. **Domain authority under lock:** Inside the transaction, `conversation::handle` rechecks the captured `conversation_state` token (`stage`, `data`, `updated_at`) and Appointment revision under `FOR UPDATE`. Stale model plans are discarded. The model cannot confirm its own cancellation (`CONFIRMAR` remains an exact standalone token check), bypass `video_enabled`, exceed the three-open-Appointment limit, or mark a Payment verified. Every outbound message is rendered from deterministic Spanish application templates.

## Configuration

`INCEPTION_API_KEY` enables Mercury 3, with `MERCURY_MODEL` defaulting to `mercury-3` when omitted. Supplied empty or non-printable values, or `MERCURY_MODEL` without `INCEPTION_API_KEY`, fail startup closed with a value-free diagnostic. The optional `CLAUDE_API_KEY` and `CLAUDE_MODEL` pair remains supported as a fallback parser when `INCEPTION_API_KEY` is absent.
