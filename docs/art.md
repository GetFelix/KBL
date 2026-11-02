# Viewer look

The viewer should read like a control room for a swarm: dark, calm when
nothing is happening, and busy in a way you can follow when everything is.
The reference is [prototype/style-frame.html](../prototype/style-frame.html),
which plays a hand-written pull request review run through the drawing code.

![The swarm, recorded from the style frame](screenshots/swarm.gif)

| Fan-out | Injection refused | Death and recovery |
|---|---|---|
| ![Fan-out](screenshots/swarm-fanout.png) | ![Injection](screenshots/swarm-injection.png) | ![Recovery](screenshots/swarm-recovery.png) |

| 50 workers, 300 tasks | Light | Phone |
|---|---|---|
| ![Burst](screenshots/swarm-burst.png) | ![Light](screenshots/swarm-light.png) | ![Phone](screenshots/swarm-phone.png) |

Full-size recordings: [swarm.mp4](screenshots/swarm.mp4) (20 s, 1600x900) and
[swarm-burst.mp4](screenshots/swarm-burst.mp4) (the generated 50-worker burst).

## Layout

- **A fixed ring.** The planner, the queue and the budget sit in the hub.
  Workers take ring slots in the order they first joined, so a worker never
  moves, live or scrubbing. Up to 16 workers get labels outside the ring (task
  and spend); up to 32 share one ring; past that a second, inner ring is offset
  by half a slot and nodes shrink to their names.
- **The board** is a column on the right, one card per key in the order keys
  first appeared, with the run log underneath. On a phone it moves below the
  ring and the ring keeps only task ids.
- **The timeline** runs along the bottom: one lane per agent with claim spans
  (light) and model calls (solid), markers for trouble, spend against the
  budget behind the lanes, and the playhead.
- **Captions** in the lower left say what just happened in plain words, so a
  still or a recording reads without knowing the legend.

## Palette

| Role | Dark | Light | Why |
|---|---|---|---|
| Work: tasks, claims, claim timers, playhead | `hsl(192, 90%, 46%)` | `hsl(192, 90%, 40%)` | Felix cyan. Work moving is the thing to watch |
| Results | `hsl(190, 85%, 74%)` | `hsl(192, 75%, 56%)` | The same hue, lighter, so a result reads as the answer to a task |
| Messages | `#a48cff` | `#6f4fe0` | The one cool colour that is not cyan; talk is not work |
| Recovered trouble: lapses, conflicts, dead letters, deaths | `#ffb547` | `#d48300` | Something went wrong and the system handled it |
| Refusals | `#ff4d5e` | `#e0233a` | Only refusals are red, so red always means "Felix said no" |
| Background | `#080d13` | `#f3f6f9` | Near black with a blue cast; light mode is a cool off-white |

Shapes carry the meaning before colour does: tasks are squares, results
circles, messages diamonds, board writes small bars. The view reads in
greyscale.

Inter for names, labels and captions; JetBrains Mono for task ids, offsets,
versions, counts and money. The cat mark is in the header and replaces the
planner in the hub once the run is done.

## Motion

- **A flight takes 600 ms of wall time at any speed.** Position is a function
  of the record's time, the playhead and the speed, so pausing freezes packets
  and seeking lands on the frame playback would have drawn.
- **Same edge within 250 ms (scaled by speed) is one packet with a count.**
  The planner's 32 posts fly as `×32`; the tasks themselves stream into the
  queue tray.
- **Past 150 packets in flight**, busy edges turn into dashed flow lines with a
  count.
- **Bookkeeping never moves.** Claim extensions, lease renewals and heartbeats
  reset arcs in place.
- **Arrivals ripple.** A claim landing on a worker, a result landing on the
  planner and a task falling back into the queue each send out a ring; a
  second attempt gets a double ring and a label.
- **Returns arc.** A lapsed or released task curves back to the queue instead
  of retracing its spoke, so it reads as going back, not as a new claim.
- **Refusals hit a shield.** The write flies to its target, stops at a red
  shield with a lock, shatters back toward the sender and leaves a label with
  the reason.
- **Death is slow on purpose.** The node dims, cracks and its claim timer
  turns amber and keeps draining until the deadline, because that is when
  Felix gives the task to someone else.
- **Quiet stretches play at 10x**, and the timeline marks them with a dotted bar under the lanes, and `prefers-reduced-motion` (or
  `?motion=reduced`) turns flights into a single flash along the edge.

## Using the style frame

Serve the repository root (`python3 -m http.server`) and open
`/prototype/style-frame.html`. Space plays and pauses, the arrows step, and
clicking anything on the scene opens the step behind it: a worker or the planner, a packet or flow line in flight, a board card, the queue tray, the budget ring, a run log row, a caption, or a timeline marker. The timeline's claim bars and message ticks only seek. `node scripts/check-steps.mjs` clicks each of these.

| Parameter | Effect |
|---|---|
| `t=<seconds>` | Render that moment of the run, paused |
| `speed=<n>` | Playback speed (1, 4, 8, 16 in the UI; any number here) |
| `theme=light` | Light theme |
| `fixture=fixture-burst.json` | The generated 50-worker burst instead of the review |
| `motion=reduced` | Flashes instead of flights |
| `flow=<n>` | Turn busy edges into flow lines past `n` packets in flight instead of 150, to see them on a fixture that never reaches 150 |
| `step=<offset>` | Open the step panel on that record |

`window.renderAt(t, speed, wall)` draws one exact frame; the recordings were
made by calling it 30 times per second of video over the DevTools protocol.
`prototype/make-fixture.py` writes both fixtures.

## Checks and frame cost

`node scripts/check-motion.mjs` holds the motion rules above to account in
headless Chrome:

- A frame playback just drew is the frame seeking to that moment draws, and a
  moment reached by stepping, by jumping from the end or from another speed is
  the same pixels. Nothing carries over between frames.
- Posts, claims and results merge by edge and window at every speed, merged
  counts add up, and a refusal, conflict, dead letter or duplicate is never
  merged.
- Only work moving flies: no extension, renewal or heartbeat becomes a packet.
- Quiet stretches are skipped only where nothing is in flight, and the
  timeline marks exactly those.
- With `flow=20` the burst turns busy edges into flow lines, and none are
  drawn at or under the threshold.
- With `motion=reduced` no packet shape is drawn.

The burst reaches 134 packets in flight at 16x, short of the design's 150, so
the flow-line rule is exercised with the lower threshold.

The backdrop and the vignette are painted once per layout and copied each
frame, because a full-size gradient and the dot grid cost more than the
packets. Drawing one frame of the 50-worker burst at 16x takes about 7 ms at
the 95th percentile on a 1440x900 canvas and about 14 ms on a 2x canvas, in
headless Chrome with software rasterising and the whole frame read back. A
laptop with a GPU canvas does less. The CI bound is 20 ms at 1x pixels: it catches a gross regression, not a
few milliseconds.
