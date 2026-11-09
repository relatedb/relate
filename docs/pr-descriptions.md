# Pull request descriptions

Every PR opened into this repository — human- or agent-authored — carries
a title and body from one template. The description is the review's front
door: the ready-flip decision, the review bots, and anyone reading the
history later all start there. A PR is one squashed commit, so the body
describes the **final diff** — never the journey that produced it.

Automation PRs with an established format of their own (the loadtest
baseline freeze workflow) keep their format.

## Title

- Conventional-commit form, identical to the squashed commit's subject:
  `type(scope): subject` — `fix(ffi): bind the empty STRING from a NULL
  zero-length buffer`. The subject says **what and where**, not how
- Tracking references as a trailing parenthetical when they exist:
  ` (LABS-1198)`, ` (ADR-0537)`, ` (LABS-990, ADR-0524)`

## Body

Write the body to a file and open with `gh pr create --body-file` — inline
bodies get mangled by shell quoting. Sections in this order; omit any
conditional section that has nothing to say. A body with empty headers is
worse than a shorter body.

````
## Summary
<1–2 sentences: what changed and why. Open with the change class — new
feature / fix / optimization / refactor / removal / other — then the what
and the why. A reader who sees only this paragraph knows what the PR is.>

## Why
<Only when the motivation needs its own evidence — measurements, an
incident, a rejection record. Otherwise the why stays in Summary.>

## Changes
- <one bullet per notable change — what it does, not a play-by-play.
  Group related edits into one bullet; a 40-bullet list means the PR
  wants splitting.>

## User-facing impact
<Only when something a user touches changed — API, CLI, config, UI,
output. What users now see or do differently. Drop the section entirely
for internal-only changes.>

## Example usage
<Only when user-facing, immediately after impact: concrete before/after —
sample input and the resulting output, in fenced code blocks. The failing
before-output and the passing after-output beat any prose.>

## Verification
<Always. What was actually run, locally, by name — commands and outcomes,
never "tested locally". Checked boxes for what ran; unchecked for what
only the ready-gate CI can prove (drafts run the fast gate only). Fixes
show A/B evidence: the failing run pre-fix, the passing run post-fix.>

Fixes <issue-id>.  <only when a ticket tracks the work>
````

## Filled example

````markdown
fix(ffi): bind the empty STRING from a NULL zero-length buffer (LABS-1198)

## Summary
Fix: `relate_params_put_string` rejected **any** NULL value buffer —
including the `(NULL, 0)` spelling of the empty string — so `""` could
never bind as a parameter through the JS client, whose FFI layer
marshals a zero-length buffer as NULL (`50N42 "null string value"`).

## Changes
- `params.rs`: NULL buffer + `len == 0` now binds the empty STRING;
  NULL + `len > 0` still refuses — `relate_params_put_null` unchanged
- `include/relate.h` + `docs/api/c/spec.md`: contract updated; client
  APIs unchanged, `""` simply binds now

## User-facing impact
JS (and any FFI layer that marshals zero-length buffers as NULL) can
now pass `""` as a parameter; every other spelling behaves as before.

## Example usage
```js
// before: 50N42 "null string value"
await db.run("CREATE (c:Customer {region: $region})", {region: ""});
// after: creates the node with region: ""
```

## Verification
- [x] `cargo test -p relate-ffi` — 48/48, including the new `(NULL, 0)`
      round-trip test (fails pre-fix, passes post-fix)
- [x] workspace clippy @1.88 `-D warnings` + fmt clean
- [ ] Bolt driver smoke against the built dylib (ready-gate CI)

Fixes LABS-1198.
````

No `Why` section — the one-line motivation fits the Summary. That is the
normal case.

## Rules of thumb

- **Bullets, not essays** — every sentence earns its place
- **No mid-story openings** — the first sentence stands alone; the reader
  has not seen your investigation
- **Evidence over adjectives** — commands, counts, numbers; "much faster"
  and "more robust" are noise
- **Honest verification** — never list a gate that was not run; unchecked
  boxes mark awaiting-CI items only
- **Provenance** — an agent-authored body ends with one attribution line
  (the tool's own footer or `This description was auto generated.`)

## Staying true across fix rounds

Fix rounds re-squash into the same commit, and the body must describe
what that commit now contains. After every re-squash, re-read the body:
fold fix-round outcomes into Changes and Verification, and drop anything
the final diff no longer does. The body never narrates rounds — it reads
as if written once, about the final tree.
