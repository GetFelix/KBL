# Changelog

## Unreleased

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
