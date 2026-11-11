# Changelog

## Unreleased

- Ingest keeps one cursor per repo. A repo that answers 403 or 404 stalls only
  itself and is reported in Needs you as `source_stalled`; state saved with the
  single cursor seeds every repo once.
- Ingest records the state of an agent's pull request: new `pr_status` and
  `source_stalled` records, a failed check sends the item to Needs you, and a
  merge closes it as done. The ingest token may now write `closed` with reason
  merged. Older trackers fold the new records as `unknown`.
- The fold caps questions, answers and links per attempt, takes an answer only
  for the open question, and the tools refuse a replayed link, oversized or
  too many evidence entries, an empty `why` and an empty edited title.
- `context_read` needs the item or its parent to be held by the caller.
- Trackers from before the `edited` and `tier_changed` records fold them as
  `unknown`, so run the new tracker before a person edits or re-tiers a proposal.
