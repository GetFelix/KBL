# Stray

**No bug left stray.**

Stray is agent work orchestration. Bugs come in from GitHub, Azure DevOps and
Jira, coding agents take them on, people review what comes back, and every
fix arrives with the reasons it was made.

## The story

Every backlog has strays. The bug filed eight months ago that nobody owns.
The flaky test everyone reruns. The ticket that is blocked by another ticket
that is blocked by a question nobody answered. They are not urgent enough to
take, so they wander, and the backlog grows by a few a day.

Stray takes them in.

- **It finds them.** Stray watches your trackers and brings every new bug
  into one queue, once, however many places it was filed.
- **It sizes them up.** A triage agent reads each one, sets a priority, links
  what it depends on, spots duplicates and writes down what it understood.
  You accept the proposal with one key or change it.
- **It finds them a home.** Coding agents claim ready work, one item to one
  agent. An agent that stalls or crashes loses its claim and the work goes to
  another. A bug that keeps beating them comes to a person instead of burning
  money.
- **It brings them back.** Each fix comes home as a pull request with the
  agent's own account: what it changed, what it checked, what it is unsure
  of. When review fills up, agents stop taking new work until people catch
  up, so your queue never buries your reviewers.
- **It leaves pawprints.** Every decision an agent made is recorded with the
  evidence behind it. Open any line of the diff and follow the trail back:
  why this file, why this approach, which test proved it. Where the agent's
  account and what it actually did disagree, Stray shows you.

## What it is not

Not another agent. Stray works with the coding agents you already use and
gives them a queue, a claim and a record. Not a tracker that waits for people
either: work moves without anyone dragging cards. And not a scoreboard.
Stray measures the flow of work, never the people doing it.

## Words we use

| Say | Instead of |
|---|---|
| agent work orchestration | AI project management, autonomous engineering |
| strays: unowned, waiting work | technical debt, backlog grooming |
| take in, claim | assign |
| pawprints: the recorded decisions behind a fix | logs, transcripts |
| my queue | inbox, notifications |
| merged fixes | velocity, throughput |

The metaphor lives on the site, in talks and in posts. The product itself
uses plain words: queue, claim, review, decision. Nobody should need the
story to use the tool.

## Lines

- No bug left stray.
- Bugs in. Reviewed fixes out.
- Every fix with its why.
- Others orchestrate agents. Stray orchestrates the work.
- Take in your strays.

## The family

Stray runs on [Felix](https://github.com/GetFelix/felix), the log that keeps
every claim exactly once and every pawprint in order. Felix is the cat that
remembers everything; Stray is the one that brings the lost ones home.

## Before launch

STRAY is a registered mark for video game software (Annapurna Games, Class 9)
and STRAY KITE has marks in classes 9 and 42 for game development. A
trademark review in classes 9 and 42 for developer software comes before any
public launch, and may point to filing a distinctive form (word plus logo, or
a descriptor). The exact .com, .ai and .dev domains are registered; plan on a
variant.
