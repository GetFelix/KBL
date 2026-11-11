# Changelog

## Unreleased

- Questions can offer options. `work_ask` takes `options` (at most 8) and a
  `default`, and a person picks one or types free text, which is stored as
  written. An answer appends a claimable `resume` item under the asking item
  that carries the question, the answer and the asker's `so_far`; the asking
  item stays out of Ready meanwhile. A person's answer can name another repo
  (`moves_to`) to create a prerequisite there instead. Triage can mark an item
  `unclear` (scope, repo or acceptance), which sends it to a person as a
  question. `work_ask` also takes `cost_micros`, charged when asking. Logs from before fold as they did; older trackers ignore the new
  fields and start no resume work.

- Standing rules per repo and per root: a person sets text and checks-before-push
  with `POST /v1/work/rules` (`rules_set`), and every claim reply and `work_next`
  returns the rules in effect with their version. The root's parts override the
  repo's; agents can read rules but never write them. Older trackers fold
  `rules_set` as `unknown`.

- Ingest keeps one cursor per repo. A repo that answers 403 or 404 stalls only
  itself and is reported in Needs you as `source_stalled`; state saved with the
  single cursor seeds every repo once.
- Ingest records the state of an agent's pull request: new `pr_status` and
  `source_stalled` records, a failed check sends the item to Needs you, and a
  merge closes it as done. The ingest token may now write `closed` with reason
  merged. Older trackers fold the new records as `unknown`.

- `work_post` takes `prerequisite`: work another repo must do first. It creates
  a proposal in the target repo's Triage, links the held item to it, and ends
  the claim as blocked without spending a try. Each link waits for a merge or
  a release (`shipped` record). The token file gains an optional `grants` map
  of the repos an agent may reach; without it an agent reaches only its own
  tree's repo. Logs from before fold as they did.

- The fold caps questions, answers and links per attempt, takes an answer only
  for the open question, and the tools refuse a replayed link, oversized or
  too many evidence entries, an empty `why` and an empty edited title.
- `context_read` needs the item or its parent to be held by the caller.
- Trackers from before the `edited` and `tier_changed` records fold them as
  `unknown`, so run the new tracker before a person edits or re-tiers a proposal.
