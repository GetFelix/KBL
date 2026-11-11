# Case study: building Felix with agents

Felix and its companion projects (felix-gateway, the client SDKs, felix-canvas,
felix-metro, Kibble itself) are built by one person directing many agents. This
page describes how that work runs today, what hurts, and how Kibble should
change it. It is the design target for Kibble: if Kibble cannot run this
project well, it is not ready for anyone else's.

## How the work runs today

One coordinator session plans the work and launches agents in lanes, each in
its own git worktree. A typical change goes like this:

1. An issue is picked from the milestone.
2. A builder agent gets a long brief: the issue, the files, and the same rules
   every time (own worktree, own build directories, no `git stash`, no
   force-push, run the docs checks before pushing, pass the repo on every `gh`
   write).
3. The builder opens a PR. A cheap reviewer reads it, and a fixer addresses
   the findings. A stronger reviewer does a final pass, and sometimes a
   second fixer follows.
4. Findings that are not worth fixing now are filed as follow-up issues.
5. Auto-merge is armed. A shell loop polls CI and merges or disarms on red.
6. Questions only the maintainer can answer (a design call, a stack choice)
   are asked in the coordinator's chat.
7. Everything above is written into a handoff file so the next session can
   resume.

On one morning (11/11), two Kibble PRs took this path. #108 went through
two review rounds and #107 through three. Together they produced seven
follow-up issues and two maintainer questions. The process works: both reviews
found real defects, including an ingest token that could write anywhere and
an agent that could steer write-back to someone else's pull request. But the
coordination around them is manual.

## What hurts

| Pain | What happens today |
|---|---|
| State lives in a handoff file | The coordinator's file is thousands of lines of "RESUME HERE (v28)" sections and addenda. A new session reads it before doing anything. |
| Rules are repeated in every brief | Each brief restates the same rules, and an agent that ignores one can do damage: one reset the main checkout, another closed a browser. |
| Review rounds are invisible | Who reviewed what, at which model tier, which findings were fixed, and which were checked by reverting the fix lives only in PR comments. |
| Follow-ups are filed by hand | Every deferred finding becomes an issue only if the coordinator remembers. Handoff notes are not tracking. |
| Polling instead of events | CI is watched by shell loops and timed wakes. A missed wake once idled a campaign for four hours. |
| Lost agents lose their context | A fixer that ends cannot always be resumed, so the next one starts from a fresh brief. |
| Order between PRs is in someone's head | #108 had to merge before #107 because they touched the same files. Nothing recorded that. |
| Spend is a guess | Usage is checked by hand ("6% left"), and the choice between a cheap and a strong reviewer is made per call. |
| The maintainer is the queue | Nobody measures how long a PR or a question waits for the one person who can unblock it. |

## How it works with Kibble

Kibble does not run models. It is the tracker the agents and the maintainer
share, and every step above becomes a record in `work.events`.

- **The board replaces the handoff file.** Ingest imports the issues of every
  repo in scope. The state of all work is the fold of the log, so a new
  session reads My queue and the board instead of a file. Nothing is lost
  when a session ends.
- **Standing rules are part of the root, not the brief.** A repo's rules
  (worktree, build directories, forbidden git commands, checks before push)
  are context every claim reads. The brief shrinks to the issue.
- **Review is work.** A first-pass review and a final review are claimable
  items under the PR's root, each with a tier that says which kind of
  reviewer should claim it. Findings are records. A finding to fix becomes a
  child item. A deferred finding becomes a proposal the maintainer accepts
  into the milestone with one key. Revert checks are recorded as evidence on
  the fix's decision.
- **Order is a dependency.** "Merge #108 before #107" is a link, so #107's
  rebase is Blocked until #108 is done, and the board shows why.
- **Events, not polling.** Ingest sees CI results and merges. A red check
  sends the item to Needs you with the failure. Retrying needs a note, so
  nobody reruns a failure to green without looking at it.
- **Handoffs survive the agent.** A claim that ends early leaves its
  decisions and `so_far`. The next claim starts from them, and a redo can
  branch from any past decision.
- **Questions go to My queue.** "Fix #2206 in hybrid placement, or let Raft
  retire it?" is a question item. The answer is a record on the issue's root,
  so the reason for the decision is found where the work is.
- **Budgets are visible.** Each root has a work and cost budget. Each claim
  records its cost, so spend per issue, per review tier and per repo is a
  query, not a guess.
- **The maintainer's wait is measured.** Time to first human action on My
  queue and In review is the number to shrink.

## Across the companion projects

Felix work often starts in a companion. A gateway gap found while building
felix-blocks becomes a felix-gateway issue and a tracking issue in the
Felix milestone. With Kibble, one root can link items in several repos, and
the companion's item is Blocked on the Felix item until it closes. The board
for one person covers every repo they own.

Often the agent finds the dependency itself, mid-task: the felix-blocks change
needs a felix-gateway feature first. The agent posts a prerequisite in the other
repo. Its held item becomes Blocked on that prerequisite without using an
attempt, and the person who owns that repo sees it in Triage. "Done" can mean
merged or released, since a companion usually needs the Felix release, not just
the merge. When the prerequisite is done, the original item is Ready again, and
the next claim resumes from the recorded decisions (#140).

## What Kibble needs for this

Already planned: dependencies (#67), budgets and lineage (#96), decision
records (#84), redo from a decision (#94), review limit (#90), time to first
human action (#91), the dashboard (#74, #132), standing rules per root or
source, delivered with every claim (#133).

New, from this case study:

- Review as claimable work with a reviewer tier, findings as records, and
  deferred findings as proposals (#134).
- Pull request and CI state from ingest, so In review and Needs you are fed
  by events (#135).
- Links across repos and sources (#136).
- Cost per claim recorded and summed per root, tier and repo (#137).
- A warning when two open items will touch the same files (#138).

## How we adopt it

1. **Shadow.** Ingest the Felix and companion repos and keep the board next
   to the current process. Compare it with the handoff file each day.
2. **Reviews and follow-ups.** Run review rounds and follow-ups through
   Kibble. The handoff file stops tracking PRs.
3. **All lanes.** Agents claim from Kibble, and the coordinator plans by
   accepting triage and setting tiers. The handoff file is retired.

Done when a new session can resume all Felix work from Kibble alone, and the
maintainer's median wait on My queue is measured and falling.
