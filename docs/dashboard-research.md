# Why the dashboard is built this way

What the evidence says about making a work view something a person acts on,
and what it does not say. Each principle names its source and how far to trust
it. Almost no study measures whether an agent dashboard raises productivity.
Most agent evidence is open-source PR mining (people choose easy tasks) or
vendor telemetry. The flow and review-latency evidence is stronger, but it
comes from human-only teams. The choices below are reasoned, and the product
should measure itself: time to first human action on My queue and In review,
before and after the view ships.

Grades: **S** is primary data or a peer-reviewed study, **M** a secondary
summary of a real study, **W** a vendor claim, a blog or a rule of thumb.

## Principles

1. **Human review is where agent output queues up.** Teams with heavy AI use
   merged 98% more PRs, PRs grew 154%, and review time rose 91% while delivery
   metrics stayed flat ([Faros](https://www.faros.ai/blog/ai-productivity-paradox-report-2025),
   M, vendor telemetry; the report itself was not retrieved). DORA 2025 finds
   AI raises throughput and instability, with small batches as the
   mitigation ([Google](https://blog.google/technology/developers/dora-report-2025/), M).
   So In review is its own stage with an age, and agents stop claiming at a
   limit.
2. **Age beats count.** An in-flight item crossing the 85th percentile of
   past cycle times is the signal to act, and cycle time itself lags
   ([Vacanti](https://www.scrum.org/resources/blog/want-be-more-predictable-do-these-two-things-every-day), M).
   Cards colour against the team's own p50 and p85 per stage.
3. **More work in progress makes every item slower.** WIP equals throughput
   times cycle time ([Little's law](https://itsadeliverything.com/littles-law-the-basis-of-lean-and-kanban), M).
   It does not locate the bottleneck, and it is an average, not a forecast
   for one item. Hence the review limit.
4. **Optimise the constraint.** Adding inventory in front of a bottleneck adds
   waiting (theory of constraints, W as sourced here, but classical). The
   footer names the binding stage and its cause instead of blaming agents.
5. **Feedback speed is a top driver of developer experience.**
   [DevEx](https://queue.acm.org/detail.cfm?id=3595878) (S/M). Google's median
   review latency is under four hours, and small changes get a first answer
   in about an hour ([Sadowski et al.](https://doi.org/10.1145/3183519.3183525), M).
   Time waiting on a human is the number to shrink, shown per item and as a
   team p50 and p85.
6. **Waiting after approval is a large, cheap slice.** One Phabricator study
   estimates 29 to 63% potential speedup from cutting acceptance-to-merge
   time ([arXiv 2203.05048](https://arxiv.org/pdf/2203.05048), S, one tool).
   So "approved, not merged" and "CI green, no reviewer" are their own states
   with one-click actions.
7. **Route, do not broadcast.** Resuming after an interruption took about 25
   minutes in one field study, but that figure includes other work done in
   between and varies enormously
   ([Mark et al.](https://addyo.substack.com/p/it-takes-23-mins-to-recover-after), S;
   the "23 minutes" folklore overstates it). The dashboard is one ordered
   queue the person opens on their own schedule, with a count in the tab
   title. It does not push every event.
8. **Triage works as an inbox with a small verb set.** Linear treats triage as
   a status and offers a curated "My issues" view
   ([triage](https://linear.app/docs/triage), [my issues](https://linear.app/docs/my-issues), S, vendor docs).
   That keyboard-first work raises output is anecdotal (W). The explicit
   triage state is the lesson that holds up.
9. **Ownership routing and dedupe cut noise.** Vendors claim suggested owners
   and grouped alerts help ([Sentry](https://docs.sentry.io/product/issues/issue-owners/),
   [incident.io](https://incident.io/blog/alert-fatigue-escalation-policies), W).
   Triage cards show a one-line reason for the tier and a possible duplicate,
   across trackers too.
10. **Agent-fleet views converge on a session list, a log and steer or stop.**
    ([GitHub Copilot agents](https://docs.github.com/en/copilot/how-tos/use-copilot-agents/cloud-agent/track-copilot-sessions), S, vendor docs.)
    Held cards link to the run and add stop, steer and reassign.
11. **Acceptance varies a lot by agent and task, and sits below the human
    rate.** In 456k agent PRs, acceptance ranged from 35% to 64% against 76.8%
    for humans, and accepted PRs merged fast
    ([AIDev](https://www.alphaxiv.org/abs/2507.15003v1), M). Selection bias is
    large. Metrics show merge and reject by task class and review rounds, not
    just throughput.
12. **Perceived speed-up can be wrong.** In a trial of 16 experienced
    developers, AI made tasks 19% slower while they believed it made them 20%
    faster ([METR](https://metr.org/blog/2025-07-10-early-2025-ai-experienced-os-dev-study/),
    S, but n=16, early-2025 tools). So the dashboard measures outcomes: merged,
    reverted, human minutes. It does not show agent activity.
13. **Smaller changes review better, but the threshold is folklore.** The
    "200 to 400 lines" ceiling traces to a study whose data could not be read
    ([Athenian](https://athenian.co/blog/why-pull-request-size-matters), W).
    DORA's small-batch result is sturdier (M). PR size is shown, and nothing
    hard-codes a ceiling.
14. **Metrics become targets.** Vendors themselves warn about gaming and
    surveillance ([Swarmia](https://www.swarmia.com/blog/introducing-software-engineering-metrics/), W, but consistent across sources).
    Metrics are team and flow level only. No rankings, no per-person counts.
15. **Reviewer attention is finite, and bot review comes from the same
    ecosystem.** Agent PRs are largely reviewed by same-vendor bots (AIDev,
    M). A bot approval is shown and never counts as review.

## What was decided from it

- The review limit has no evidence behind any default number. The research
  suggested two or three per active reviewer, which it marked as a guess. It
  is a setting, a soft one: it pauses claims and never blocks triage.
- The ranking of what to build first (review queue, then the question inbox,
  then bulk triage, then unblock, failure triage) is judgement built on
  principles 1, 5, 6 and 8. It is not a measured result.
- Avoided on purpose: individual rankings, agent "busy %" as productivity,
  averages without percentiles, a single velocity number, raw acceptance rate
  as a target, hard limits that block triage, a notification on every state
  change.
