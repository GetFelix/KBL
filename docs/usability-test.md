# Usability test: does the dashboard feel familiar?

The bar for the dashboard is that someone who uses Linear, Jira, GitHub
Projects or Sentry feels at home within minutes
([#132](https://github.com/GetFelix/KBL/issues/132)). This is the
script for checking that with five people, before the live dashboard is built
from the style frame.

It tests the frame, `prototype/intake-frame.html`, on a fixed fixture. Nothing
talks to Felix, so every participant starts from the same morning.

## What passes

- Every participant finishes the four core tasks unaided: answer a question,
  accept a triage proposal, review and merge, retry a failure.
- Every participant finishes the first task in under 2 minutes.
- No convention task (5 to 9) fails for three or more participants.

Unaided means the moderator said nothing between reading the task and the
participant saying they are done.

## Who

Five people who use Linear or Jira every working day and who review pull
requests. None of them has seen KBL (Kanban By Log, pronounced Kibble). Aim for at least two from each tool,
and at least one who works mostly from the keyboard and one who mostly uses the
mouse.

Screening questions:

1. Which tracker did you use yesterday? (Linear or Jira, else stop.)
2. How many pull requests did you review last week? (One or more.)
3. Have you seen KBL or its dashboard before? (No.)

## Setup

One session takes 30 minutes: 5 to brief, 20 for tasks, 5 for questions.

1. Serve the repository and open the frame in a new private window, so nothing
   is saved from the last session:

   ```bash
   python3 -m http.server 8000
   # http://localhost:8000/prototype/intake-frame.html?fixture=fixture-work.json
   ```

2. Use a laptop at 1440 by 900 or larger. Let the participant pick light or
   dark from the half-moon button before the first task.
3. Record the screen and the voice, with consent. Open "Recorded by your
   actions" at the bottom of the page: it lists the record each action writes,
   and it is how the moderator checks a task.
4. Reload the page between participants.

Read this to the participant:

> This is a dashboard for a team whose coding agents fix bugs. You are Dana, a
> developer on that team, and it is the start of your day. I will read you some
> tasks. Do them the way you would in your own tracker. Think out loud. I am
> testing the page, not you, and I cannot help while you work. Say "done" when
> you think a task is finished, or "I give up".

Do not explain the page, name a key, or point. If asked a question, answer
"What would you try?".

## Tasks

Read each task aloud, start the timer at the last word, and stop it at "done".
A task that passes its time limit counts as failed, but let the participant
finish so the later tasks still make sense.

| # | Read aloud | Passes when | Limit |
|---|---|---|---|
| 1 | "An agent is stuck on a question about a duplicate delivery. It cannot tell which repo the fix belongs in. You know it is the gateway. Tell it." | An `answered` record on AB#4377 with `chosen=felix-gateway` | 2 min |
| 2 | "An agent has suggested priorities for new bugs. Accept the suggestion for the put_if bug." | A `triaged` record on #808 | 2 min |
| 3 | "One of the new bugs looks like one you already have. Deal with it." | A `rejected` record on AB#4408 with `dup` | 2 min |
| 4 | "A pull request is waiting for your review: the heartbeat interval one. Assume you read it and it is fine. Approve it. You then merge it on GitHub. Make KBL show that." | An `approved` and then a `done` record on #789 | 3 min |
| 5 | "An agent gave up on a flaky cache test. Send it round again, and tell the next agent to pin the clock." | A `retried` record on #774 with a note | 2 min |
| 6 | "You did not mean to do that last one. Take it back." | The `retried` record is gone and #774 is in the queue again | 1 min |
| 7 | "Three of the new bugs can wait until this afternoon. Put all three off in one go." | Three `snoozed` records from one action | 2 min |
| 8 | "Show only the most urgent GitHub work, as a board, and keep that so you can come back to it tomorrow." | A board filtered to P0 and P1 and GitHub, and a new entry under Views | 3 min |
| 9 | "Without the mouse, find the bug about group recovery time and open it." | #803 is open in the side panel | 2 min |
| 10 | "Where would you look to learn the keyboard shortcuts?" | The shortcut list is open, by `?`, the help button or the palette | 1 min |

Tasks 1, 2, 4 and 5 are the core tasks. Task 1 is always first. Keep the order:
task 6 undoes task 5.

For each task, note:

- the time, and pass, fail or gave up;
- the first thing they tried, and whether it worked;
- whether they used the mouse, a key, or the palette;
- every key they pressed that did something they did not expect;
- any word on the page they asked about or read aloud twice.

The fourth note matters most. A key that does something other than what a
Linear or Jira user expects is a defect in the key map, even if they recover.

## Questions afterwards

1. Which tracker did this remind you of, and where did it differ?
2. Was there a key you reached for that did nothing, or did something else?
3. What did you expect the numbers on the cards to do?
4. Which word on the page was unclear?
5. On a scale of 1 to 5, how soon could you use this for your own reviews
   without help?

## Scoring sheet

One row per participant:

| Participant | Tracker | Keyboard or mouse | T1 time | T1 | T2 | T3 | T4 | T5 | T6 | T7 | T8 | T9 | T10 | Unexpected keys | Q5 |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| P1 | | | | | | | | | | | | | | | |
| P2 | | | | | | | | | | | | | | | |
| P3 | | | | | | | | | | | | | | | |
| P4 | | | | | | | | | | | | | | | |
| P5 | | | | | | | | | | | | | | | |

## After the test

- Post the filled sheet and the list of unexpected keys on #132.
- A core task that anyone failed is fixed in the frame and tested again with a
  new person. The bar is five passes, not four out of five.
- A convention task that three or more people failed gets an issue and a fix
  before the live build starts.
- Anything else that tripped two or more people gets an issue.
- Update the key map in [work-tracker.md](work-tracker.md#my-queue) in the same
  change as any key that moves.

## What this does not test

- Live data, latency or errors from a real tracker.
- The decision replay, which has its own page:
  [decision-replay.md](decision-replay.md).
- Whether the dashboard makes a team faster. That is measured after it ships:
  time to the first human action on My queue and on In review
  ([dashboard-research.md](dashboard-research.md)).
