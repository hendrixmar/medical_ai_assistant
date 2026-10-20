# Issue tracker: GitHub

Issues and specs live in GitHub Issues for
`hendrixmar/medical_ai_assistant`:
https://github.com/hendrixmar/medical_ai_assistant/issues

Use the `gh` CLI. Pass `--repo hendrixmar/medical_ai_assistant`
explicitly so operations target this repository even when the current
directory has no Git remote.

## Conventions

- Create: `gh issue create --repo hendrixmar/medical_ai_assistant --title "..." --body "..."`
- Read: `gh issue view <number> --repo hendrixmar/medical_ai_assistant --comments`
- List: `gh issue list --repo hendrixmar/medical_ai_assistant --state open --json number,title,body,labels,comments`
- Comment: `gh issue comment <number> --repo hendrixmar/medical_ai_assistant --body "..."`
- Apply a label: `gh issue edit <number> --repo hendrixmar/medical_ai_assistant --add-label "..."`
- Remove a label: `gh issue edit <number> --repo hendrixmar/medical_ai_assistant --remove-label "..."`
- Close: `gh issue close <number> --repo hendrixmar/medical_ai_assistant --comment "..."`

Use `--body-file` with a file or standard input for multiline bodies.
Apply appropriate label and state filters when listing issues.
Use `docs/agents/triage-labels.md` for the label vocabulary.

## Pull requests as a triage surface

**PRs as a request surface: no.**

GitHub shares issue and PR numbers. When a reference is ambiguous,
resolve it with `gh pr view <number> --repo hendrixmar/medical_ai_assistant`
and fall back to `gh issue view` if it is not a PR.

## When a skill says "publish to the issue tracker"

Create a GitHub issue in `hendrixmar/medical_ai_assistant`.

## When a skill says "fetch the relevant ticket"

Run `gh issue view <number> --repo hendrixmar/medical_ai_assistant --comments`.

## Wayfinding operations

Used by `/wayfinder`. The map is one issue with child issues as tickets.

- Map: an issue labelled `wayfinder:map`, holding Notes,
  Decisions-so-far, and Fog.
- Child ticket: link it to the map as a GitHub sub-issue. Where
  sub-issues are unavailable, list it in the map's task list and add
  `Part of #<map>` at the top of the child's body.
  Use `wayfinder:<type>` labels:
  `research`, `prototype`, `grilling`, or `task`.
- Blocking: use native GitHub issue dependencies when available:
  `gh api --method POST repos/hendrixmar/medical_ai_assistant/issues/<child>/dependencies/blocked_by -F issue_id=<blocker-db-id>`.
  Obtain the blocker's numeric database ID with
  `gh api repos/hendrixmar/medical_ai_assistant/issues/<blocker-number> --jq .id`.
  If dependencies are unavailable, use a `Blocked by: #<n>, #<n>`
  line at the top of the child's body.
  A ticket is unblocked when all blockers are closed.
- Frontier: inspect the map's open children in map order. Select
  the first unassigned child with no open blockers.
- Claim: assign the child to the driving developer before work:
  `gh issue edit <number> --repo hendrixmar/medical_ai_assistant --add-assignee @me`.
- Resolve: comment with the answer, close the child, and append
  a summary and link to the map's Decisions-so-far.
