# The language model only parses; the application states every fact

Contacts write free-form Spanish, so an LLM interprets their messages, but only into a closed set of intents and arguments that the booking rules then validate. Every message that states a fact (times, prices, bank details, CLABE, Meet links, Appointment status) is rendered from application templates, never from model output. This makes "the model cannot invent availability, bank details, verification decisions, or successful actions" true by construction instead of by prompt.

## Considered Options

- Menu-only bot with no LLM: predictable, but a poor fit for free-text requests like "¿tiene algo el jueves en la tarde?".
- LLM as a free-form agent that writes replies and calls tools: cannot guarantee that factual messages are correct.
