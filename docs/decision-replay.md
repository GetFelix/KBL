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
record kinds are added.

| Kind | Written by | Details |
|---|---|---|
| `decision` | `felix-mcp` (`decide` tool) | The question, the options considered, the one chosen, why, and the offsets of the steps it rests on |
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

- **So far:** what had happened up to here, in two or three sentences.
- **Right now:** the decision being made at this point, or the step under way.
- **Why:** the reason the agent recorded, and the evidence it rested on.

It explains only what was known at that point. The explanation for a moment
is built from the log up to that offset and nothing after it, so it never
spoils the outcome or credits the agent with something it found out later.
Every sentence cites an offset; a claim with no offset is not shown.

Nothing appears while dragging, only on release, so scrubbing stays fast and
the panel does not flicker. It must appear in under a second, so the "so far"
summary is built as the run goes, one per decision, and kept as an artifact
next to the run. Letting go between two decisions shows the nearest one at
once and refines it for the exact step. A summary for a given offset never
changes, so it is cached and the next reviewer gets it instantly.

**Ask.** A reviewer can also ask a question ("why not fix it in the client?").
The answer comes from a model that sees only this item's log and must cite an
offset for every claim. An answer it cannot cite says so.

## Claims checked against steps

A decision is the agent's account; the steps are what happened. The story
view puts them side by side and flags where they disagree: a decision that
says the tests pass with no test run after the last edit, an edit with no
decision behind it, a cited offset that does not show what the decision says
it shows. These flags matter more than the story itself, because they tell a
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
full scrub and Ask follow replay in M7.
