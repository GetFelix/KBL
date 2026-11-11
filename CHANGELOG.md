# Changelog

## Unreleased

- The fold caps questions, answers and links per attempt, takes an answer only
  for the open question, and the tools refuse a replayed link, oversized or
  too many evidence entries, an empty `why` and an empty edited title.
- `context_read` needs the item or its parent to be held by the caller.
- Trackers from before the `edited` and `tier_changed` records fold them as
  `unknown`, so run the new tracker before a person edits or re-tiers a proposal.
