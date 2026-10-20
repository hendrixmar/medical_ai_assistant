# Domain Docs

This repo uses a single-context layout.

## Before exploring, read these

- Root `CONTEXT.md` for domain vocabulary and context.
- ADRs in `docs/adr/` relevant to the area being explored.

If these files do not exist, proceed silently. Do not suggest creating
them upfront. The `/domain-modeling` skill creates them lazily when
terms or decisions are resolved.

## File structure

/
├── CONTEXT.md
└── docs/
    └── adr/
        └── NNNN-decision-title.md

This configuration does not create a glossary or ADRs.

## Use the glossary's vocabulary

When naming domain concepts in issue titles, proposals, hypotheses,
or tests, use the terms defined in `CONTEXT.md`. Avoid synonyms the
glossary explicitly rejects.

If a needed concept is missing, reconsider whether it belongs to
the project's vocabulary. Note genuine gaps for `/domain-modeling`.

## Flag ADR conflicts

If a proposal contradicts an existing ADR, surface the conflict
explicitly rather than silently overriding the decision. Identify
the ADR and explain why reopening it may be worthwhile.
