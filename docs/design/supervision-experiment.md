# The supervision experiment

[supervision.md](supervision.md) says KBL is worth using only if one engineer can supervise far more agent work
with it than without it. This page is the protocol that tests that claim. Nothing here has been run.

## Hypothesis

With the same engineer and the same kind of work, KBL with 8 to 12 agents delivers several times the merged,
trusted work per day of Claude Code sessions driven directly, at the same or fewer engineer minutes per merged
fix and with no more escaped defects.

KBL with 4 agents is a middle arm. It shows whether any gain comes from KBL itself or only from running more
agents.

The claim fails if arm 3 is less than about 2 times arm 1 in merged fixes per day, or if engineer minutes per fix
or escaped defects are worse. A result in between is reported as unclear, not as a pass.

## Conditions

One engineer, three arms, equal calendar time per arm:

1. Claude Code sessions driven directly. No KBL.
2. KBL with 4 agents.
3. KBL with 8 to 12 agents.

Held the same in every arm: the engineer, the agent model and its settings, the repo, the checks a merge needs,
the review bar (the engineer reads and approves every merge), and working hours. The agent count is the only
other thing allowed to differ, and in arm 1 it is whatever the engineer chooses to run.

## Choosing and assigning work

1. Before the first run, fix the pool: open issues in one repo that are fixes of a size an agent can finish in a
   session, filtered by a rule written down in the run's record. The rule never looks at how hard an issue seems
   after reading it.
2. Drop issues that depend on each other, so one arm cannot unblock another.
3. Sort the pool into size classes (small, medium) and kinds (bug, test, docs). Within each group, shuffle with a
   seed that is published before assignment, then deal the issues to the arms in turn. Each arm gets the same mix.
4. The arms run in blocks of days. Their order is also shuffled by the seed, so a learning effect and a bad week do
   not line up with one arm.
5. Assignment is not changed after work starts. An issue that turns out to be unworkable stays in its arm as a
   failure.

## What is measured

KBL arms are read from `work.events` with `kbl-tool work metrics` (below). Arm 1 has no KBL records, so the
figures it can share are taken from GitHub and from a hand log. Where a figure exists only with KBL, the table
says so, and it is not compared across arms.

| Measure | Arm 1 (no KBL) | Arms 2 and 3 (KBL) |
|---|---|---|
| Merged fixes per day | Merged PRs that close an assigned issue, from GitHub | `summary.done_per_day`: Metrics tab, throughput |
| Engineer minutes per merged fix | Hand log divided by merged fixes | `--engineer-minutes` divided by `summary.done`; same hand log |
| Rework | PRs that needed a fix-up after the first review, or an issue reopened, from GitHub | `redos_per_done`, `reopened_per_done`, `failed_attempts_per_done`: Metrics tab, rework |
| Escaped defects | Issues filed within 14 days that name the merged PR, from GitHub | Same |
| Confidence | Engineer rates each merge 1 to 5 when approving | Same |
| Items a person touched, and records a person wrote, per done item | Not recorded | `touched_share_permille`, `touches_per_done`: Metrics tab, supervision |
| Time waiting on a person | Not recorded | `wait_on_person_p50_ms`, `wait_on_person_p85_ms` |
| Cost per done item | Provider usage report divided by merged fixes | `cost_per_done_micros`: recorded usage priced by the fold |

Notes on what these mean:

- The hand log is a start and stop time kept by the engineer for every stretch spent on the experiment, written
  down at the time, not rebuilt later. It is the one figure the tracker cannot see, and the most open to error.
- The Metrics tab counts items "done" of any kind, including reviews and triage. The harness reports the same
  figure. In the arms, the pool is fixes only, but follow-up items KBL creates (a review, a redo) are counted as
  the Metrics tab counts them. Publish both the tab's count and the count of merged fixes from GitHub, and use
  the GitHub count for merged fixes per day in every arm.
- Cost in arm 1 comes from outside KBL and prices tokens differently, so a cost comparison across arm 1 and the
  others is only indicative. Compare arms 2 and 3 directly.
- Escaped defects need 14 days after the last merge. The experiment is not complete until then.
- The Metrics tab is team-wide and has no per-person figure ([flow-metrics.md](../flow-metrics.md)). The harness
  does not add one. With a single engineer that makes no difference, but the numbers are not a judgement of a person.

## The export

```text
kbl-tool work metrics --label arm-3-block-1 --days 7 --to-ms 1795000000000 --engineer-minutes 640 --format json
```

It reads `work.events` with the same Felix credentials as `kbl-tool work export`, folds it with the dashboard's own
fold and prints the report for the window ending at `--to-ms`. It writes nothing to Felix or to the tracker. `json`
holds the full Metrics report and a flat `summary`; `csv` holds `label`, the window and the `summary` as
`name,value` rows. A figure with nothing done in the window is empty, not zero.

Run it at the end of each block and keep the files with the hand log and the GitHub lists. The window is whole days
back from `--to-ms`, up to 90, so a block longer than that is exported in parts. Items done count in the window
where they last became done, so a block's export is final only after nothing from it can still be reopened.

## How many items

One engineer is one subject, so the sample is items and days, not people. The aim is a ratio of several times, so
the test only has to tell "about 1x" from "about 3x or more".

- Plan for 30 or more merged fixes per arm and at least 10 working days per arm. With throughput this different,
  that is enough to see the gap in a bootstrap interval. It is not enough to find a 20 percent difference, and the
  write-up does not claim to.
- Compare arms with a bootstrap over days for fixes per day and over items for minutes per fix, and report the
  interval, not only the point estimate.
- If an arm falls short of 30 merged fixes, say so and report the interval anyway. Do not extend the run until the
  result looks right. Decide the stopping date before the first block.

## Threats to validity

- **One engineer.** Results may not carry to others, and the engineer wrote the tool. Say so in the write-up. A
  second engineer on a smaller run is the best answer and is not part of this plan.
- **The engineer knows the hypothesis.** Confidence ratings and the hand log can drift toward the hoped-for result.
  Counts from GitHub and from the log do not depend on that. Ratings are reported but carry less weight.
- **Learning and novelty.** Arm 1 is a skill the engineer already has. KBL is newer to them and also new to its
  other users. Run a few practice days in each arm first and drop them. Block order is shuffled, so remaining
  learning does not favor one arm.
- **Carry-over.** Merged work in one block changes the code in the next. The no-dependency rule in selection and
  the shuffled order limit it.
- **Unequal issues.** Stratified shuffling balances size and kind, not true difficulty. The bootstrap over items
  shows how much that matters.
- **Different agent behavior.** Model updates mid-run change throughput. Pin the model and note any change.
- **Arm 1 has fewer records.** Its figures come from GitHub and a hand log, so they are coarser. The table above
  marks which comparisons are like for like.
- **Merges KBL rework hides.** An item KBL redoes counts once. Compare `redos_per_done` and GitHub rework side by
  side so effort spent on redos is not lost.
- **Counting what is easy.** More merged fixes can come from taking easy work first. The size-class mix per arm is
  published, and escaped defects and confidence are read together with the count.
- **Machine and network load.** Record the hardware and any outage. A block with an outage is kept and flagged.

## Publishing the result

- Before the first block, publish this protocol, the seed, the pool rule, the stopping date and what counts as a
  pass. Changes after that are listed, dated and explained in the write-up.
- After the last block and the 14-day wait, publish every export file, the hand log, the GitHub lists and the
  assignment, in one place, along with the write-up.
- The write-up reports all arms and all measures in this page, whether they favor KBL or not. A null or negative
  result is published with the same prominence as a positive one, and the plan for Scale work in
  [supervision.md](supervision.md) follows the result: if KBL does not pass, Scale work stays parked.
- Blocks or items that are dropped are listed with the reason. Nothing is dropped because of how it came out.
