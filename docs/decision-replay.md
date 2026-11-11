# Why it was done this way

A reviewer opens a pull request an agent wrote and sees a fix that works, or
looks like it works, but not why it is shaped that way. Why this file and not
the one the issue named? Why a retry here instead of a lock? Did it run the
test it says it ran? Today the answer is a long transcript nobody reads, or
nothing.

Felix Agents already keeps every step of a run in one ordered log, so the
history is there. This adds the two missing parts: the agent records its
decisions as it makes them, and the dashboard reads that history back as a
story a reviewer can follow, check and question.

## What gets recorded

The run log already has every model call, tool call, claim and message. Three
record kinds are added, plus the `question` work kind described under Ask.

| Kind | Written by | Details |
|---|---|---|
| `decision` | `felix-mcp` (`decide` tool) | `so_far` (a short running narrative of the work up to here), the question, the options considered, the one chosen, why, and the offsets of the steps it rests on. `decide` refuses a call without `so_far` |
| `edit` | `felix-mcp` | File, hunk, and the offset of the decision it carries out |
| `human_input` | the tracker | An answer, a review comment or a change of priority, with who gave it |

An agent calls `decide` at the points that shape a fix: what it believes the
cause is, which approach it takes, an alternative it rejects, and a change of
scope. The worker prompt asks for it at those points and nowhere else; a
decision per keystroke is as unreadable as none.

A decision cites evidence by offset: the test it ran, the log it read, the
file it searched. Offsets are stable, so a citation still resolves a year
later, and Felix records which agent wrote each step, so a decision can only
cite steps that really happened in the order they happened.

## Reading it back

An item's history spans every attempt and every person who touched it:
attempt one gave up because the test would not reproduce, a person answered
the question, attempt two took another approach. Three views, all folds of
the same log:

- **Story.** The decisions in order, grouped into understand, reproduce,
  cause, fix and verify. Each line opens the steps behind it. This is the
  default and fits on one screen for most fixes.
- **Why is this line here.** From a hunk in the diff to the `edit` that made
  it, the decision that edit carries out, and the evidence that decision
  cites. This is the question reviewers actually ask.
- **Replay.** The run's timeline, filtered to this item, scrubbed step by step
  as the swarm view does.

**Explained where you stop.** When the reviewer lets go of the scrubber, or
steps to a point with the arrow keys, a short panel says three things about
that moment:

- **So far:** the `so_far` the agent recorded with its latest decision.
- **Right now:** that decision, or the step under way.
- **Why:** the reason the agent recorded, and the evidence offsets it rests on.

Nothing is generated. The panel shows the fields of the nearest decision at or
before the playhead, so it is instant and cannot leak hindsight: a decision
recorded later is not in the log up to that offset. This is a property of the
design, not a rule a summarizer has to follow. Every line shows the offsets
it rests on.

Nothing appears while dragging, only on release, so scrubbing stays fast and
the panel does not flicker.

**Ask.** A reviewer can post a question on the item ("why not fix it in the
client?"). It is a work item of kind `question`. Any agent claims it, reads
the item's log and completes it with an answer that cites offsets. The answer
is a record on the item, like any other, and the tracker and dashboard
generate nothing.

## Redo from here

A reviewer who disagrees with where a fix went can rewind to the decision
where it went wrong, say what should change, and have an agent take it again
from there. This is not a rerun: model calls are not repeatable, so the old
attempt stays exactly as it happened and the redo is a new attempt that
branches from that point.

- **Where it can start.** At a decision. Agents commit their work at every
  decision, so the code as it stood at any decision can be restored exactly.
  Rewinding between two decisions starts from the one before.
- **What the new attempt knows.** The fork decision's recorded `so_far`, the reviewer's note, and by default a short account of what
  the first attempt did afterwards and why the reviewer rejected it. The
  reviewer can untick that last part to get a clean second opinion.
- **What gets recorded.** The note is a `human_input` with who wrote it, and
  the new attempt starts with a `forked_from` record naming the item, the
  attempt and the offset. The timeline shows the attempts as branches, so
  anyone replaying later sees who steered, where and why.
- **What happens to the first attempt.** Its pull request becomes a draft and
  stays linked. The two attempts sit side by side (diff, checks, cost, time)
  until the reviewer picks one. The other is closed with the reason.
- **Cost.** The redo shows its estimated cost before it starts and counts
  against the same budget. The cost of a fix includes every branch it took.

- **The handoff is a narrative the reviewer can edit.** The new agent does
  not get the raw log. It gets the fork decision's recorded `so_far`, plus
  the reviewer's edits: what was found, what was tried, what was decided and
  why. The reviewer edits it before handing off, striking an assumption that was
  wrong, correcting a fact, adding what they know, and then writes the
  instruction for the new attempt. The edited narrative and the instruction
  are what the agent starts from, and both are recorded, so the replay shows
  exactly what the second agent was told and how it differed from what the
  first one believed.
- **The handoff travels with the work.** The edited narrative and the
  instruction are stored together as an artifact attached to the item, and
  the redo goes back on the ready queue as a new attempt that points to it,
  with the commit to start from. Any agent picks it up through the normal
  claim, so a redo gets the same once-only handout, extension and give-up
  rules as any other work. Nothing about the first attempt is deleted: its
  branch, its pull request (now a draft) and its run stay in place for the
  comparison.
- **Redos are limited.** An item can be redone at most three times (a
  setting). Each redo is a sign the item was misunderstood or too big, so
  after the limit it goes to Needs you with its branches attached and two
  choices: take it yourself, or split it. At most two attempts are open at
  once, the current one and the one being compared; picking one closes the
  other.

"Request changes" on a pull request is the same action from the last
decision: the comment becomes the note.

### The fork in the dashboard

The item's timeline is a path, not a line. Decisions are dots along it, and
a redo branches off at the decision it started from, marked with who steered
and the first line of their note. Each branch ends in its outcome: merged,
in review, rejected, or still running. Selecting a branch replays it; the
fork point shows the edited narrative next to what the first attempt had
believed. Board and queue cards for an item with more than one attempt carry
a small "2 paths" mark, and the review panel compares the open attempts side
by side.

## Claims checked against steps

A decision is the agent's account; the steps are what happened. The story
view puts them side by side and flags where they disagree: a decision that
says the tests pass with no test run after the last edit, an edit with no
decision behind it, a cited offset that does not show what the decision says
it shows. The flags are deterministic rules over the records, not a model's opinion.
These flags matter more than the story itself, because they tell a
reviewer where to look hardest. Reviewers can mark a story accurate or not,
which feeds the trust numbers on the metrics tab.

## The Felix mapping

- One run log shard gives every step one order across agents, people and
  attempts, which a story needs.
- Offsets are the citations. Range reads and the fold give any point in the
  history in under a second (demonstration 4).
- The recorded publisher makes authorship verified, not claimed. Through
  felix-gateway this needs gaps 7 and 9.
- Large outputs (test logs, model responses) are artifacts by hash, so a
  story stays small and opens the full output on demand.
- Retention: an item's records are kept while its issue or pull request is
  open and for a set time after; the export from `agents-tool` takes them
  with it.

## Limits

- An agent can record a reason that is not its real one. The checks above
  catch some of that, not all. The story is evidence for a reviewer, not a
  replacement for reading the diff.
- Secrets in tool output would end up in the log. `felix-mcp` redacts known
  secret patterns before writing, and artifacts follow the run's grants.

## Order

`decide`, `edit` and `human_input` land with the first real work (M3), since a
history that was not recorded cannot be replayed later. The story view and
"why is this line here" come with the dashboard in M4, built on the fold. The
full scrub and Ask (a `question` work item) follow replay in M7.
