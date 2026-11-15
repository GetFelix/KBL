# KBL

**Big bugs, bite-sized fixes.**

KBL (Kanban By Log, pronounced Kibble) is agent work orchestration. Bugs come in from GitHub, Azure DevOps
and Jira, agents break them into pieces an agent can finish through KBL, coding agents
take them on, people review what comes back, and every fix arrives with the
reasons it was made.

## The story

Agents are hungry and fast, and most backlogs are terrible food. A bug report
says "group state is lost sometimes". Hand that to an agent and you get a
guess, a sprawling diff, or a confident fix for the wrong thing. Meanwhile the
backlog grows by a few a day and nobody can say which bug is waiting on what.

KBL turns the backlog into something agents can eat.

- **It gathers the work.** KBL watches your trackers and brings every new
  bug into one queue, once, however many places it was filed.
- **Agents break it down.** Triage is work an agent picks up through KBL: it proposes a
  priority, spots duplicates, links what each bug depends on and splits big
  ones into pieces an agent can finish and a person can review in one
  sitting. You accept the proposal with one key or change it.
- **It feeds the agents.** Coding agents claim ready pieces, one piece to one
  agent. An agent that stalls or crashes loses its claim and the piece goes to
  another. A piece that keeps beating them comes to a person instead of
  burning money.
- **It brings the fixes back.** Each fix comes home as a pull request with the
  agent's own account: what it changed, what it checked, what it is unsure
  of. When review fills up, agents stop taking new work until people catch
  up, so the queue never buries your reviewers.
- **It leaves pawprints.** Every decision an agent made is recorded with the
  evidence behind it. Open any line of the diff and follow the trail back:
  why this file, why this approach, which test proved it. Where the agent's
  account and what it actually did disagree, KBL shows you.

## What it is not

Not another agent. KBL runs no models itself: it works with the coding
agents you already use and gives them a queue, a claim and a record. Every
judgement it shows, from triage to a fix, is work an agent or a person did.
Not a tracker that waits for people either: work moves without anyone dragging cards. And not a scoreboard.
KBL measures the flow of work, never the people doing it.

## Mark

A big bug with one bite-sized piece lifted out of its corner: the piece an
agent can take away and finish. Two shapes on a 64 grid with four-unit gaps,
so every edge lands on a whole pixel at 16 px.

| Colour | Hex | Use |
|---|---|---|
| Amber | `#C86A1E` | the bite, accents, graphics and large text on either background |
| Dark amber | `#A4520F` | amber text on white (amber itself is 3.8:1 there) |
| Graphite | `#14171C` | the body on light backgrounds |
| Paper | `#ECEAE6` | the body on dark backgrounds |

The files are in this folder. `kbl-mark.svg` and `kbl-lockup.svg` draw the
body in `currentColor`; the `-light` (graphite) and `-dark` (paper) variants
are for places that cannot set it. `scripts/render-mark.mjs` renders the PNGs.

Keep the mark at 16 px or larger and the lockup at 24 px tall or larger. The
bite is always amber; do not recolour, outline or rotate it.

## Words we use

| Say | Instead of |
|---|---|
| agent work orchestration | AI project management, autonomous engineering |
| bite-sized: a piece one agent finishes and one person reviews | ticket, story points |
| claim | assign |
| pawprints: the recorded decisions behind a fix | logs, transcripts |
| my queue | inbox, notifications |
| merged fixes | velocity, throughput |

The metaphor lives on the site, in talks and in posts. The product itself
uses plain words: queue, item, claim, review, decision. Nobody should need
the story to use the tool.

## Lines

- Big bugs, bite-sized fixes.
- Feed your agents real work.
- Bugs in. Reviewed fixes out.
- Every fix with its why.
- Others orchestrate agents. KBL orchestrates the work.

## The family

KBL runs on [Felix](https://github.com/GetFelix/felix), the log that keeps
every claim exactly once and every pawprint in order. Felix remembers
everything; KBL makes sure the work gets eaten.

## Before launch

"Kibble" alone searches as pet food, so the name is written KBL and travels with its descriptor
("KBL, agent work orchestration") and a variant domain. No software mark
for KIBBLE turned up in a first search (nearest: KWIBBLE in classes 35 and 42,
KRADLE pending for AI agent testing); a trademark review in classes 9 and 42
comes before any public launch.
