# Supervision: one engineer, many agents

## The claim

Scheduling work across many agents is a solved problem. KBL is worth using only if it lets one engineer
**confidently supervise far more concurrent engineering work** than they could by driving agent sessions
themselves. That depends on the human and provenance side: seeing what is happening, deciding quickly, and trusting
what was done. This page is the plan to build that and then measure it.

For a few agent sessions, KBL is probably unnecessary. For lots of durable agent work that people must understand,
trust and steer, it should pay for itself. The experiment below decides whether it does.

## What a supervisor needs

Found by running KBL v0.1.0 over the Felix backlog with Claude Code agents, where scheduling worked and supervision
did not:

| Need | Issue | Status |
|---|---|---|
| A question parks the item and frees the agent, and is answered on the board, never in the agent's chat | #375 | Done |
| See each issue's stage and its triage step | #373 | Done |
| A board that shows who is working, why something is blocked, and the upstream id, with child work nested | #374, #386 | #374 done; #386 (follow-ups) planned |
| An item panel that says what is happening now, with a live activity timeline, PR and checks, and attempts | #369 | Done |
| One inbox for everything waiting on a person, answered in one action | #388 | Done |
| Summary, diff and checks beside each finished attempt, with a trust mark | #389, #307 | #307 (trust numbers) done; #389 planned |
| Steer the queue: skip work that cannot run now, ask for a kind of work, soft links, skip triage per rule | #376, #377, #378, #384 | #377 and #384 done; #376 and #378 planned |

These form the milestone "Supervision: one engineer, many agents".

## The experiment

The same engineer and the same slice of the Felix backlog (comparable issues, assigned at random), in three arms
of equal time:

1. Claude Code sessions driven directly, without KBL.
2. KBL with 4 agents.
3. KBL with 8 to 12 agents.

Measured per arm:

- merged fixes per day
- human minutes per merged fix
- time an item waits on a person
- reject and rework rate
- defects found in the two weeks after merge
- the engineer's confidence in each merge, 1 to 5

KBL passes if arm 3 delivers several times the merged, trusted work of arm 1 at the same or lower human minutes per
fix, with no more escaped defects. The results get written up either way. The protocol, with how each measure is read
from recorded fields, is in [supervision-experiment.md](supervision-experiment.md).

## What waits

The Sources milestones that scale ingest and connecting sources (#342 to #354) and the many-agent work (#34 to #36)
are parked in "Scale (after supervision is proven)". Sources milestone 1 (#341) is done. Scale work resumes if the
experiment shows KBL is worth using.
