#!/usr/bin/env python3
"""Writes the dashboard's fixtures.

fixture-work.json is a morning: three upstream trackers, a blocked chain
behind one held item, items waiting on a person, held items with their agents,
agent PRs in review, triage proposals, all four tiers, and two weeks of
finished work behind it for the metrics tab. fixture-work-capped.json is the
same morning with the review limit reached. fixture-work-burst.json is
generated: a few hundred items, most of them arriving in the last hour, to
check the view stays readable. Same input, same files.

Both use display fields (seconds, short ids). The viewer moves to the real
work.events format when it reads from Felix.
"""
import json
import random
from pathlib import Path

HERE = Path(__file__).parent
MIN = 60
HOUR = 3600
DAY = 86400
NOW = 14 * DAY  # the fixtures end here; "ago" counts back from it

VIEWER = "dana"
PEOPLE = ["dana", "kim", "ravi"]
REVIEW_CAP = 6

SOURCES = {
    "github": {"label": "GitHub", "tag": "GH", "scope": "GetFelix/felix"},
    "ado": {"label": "Azure DevOps", "tag": "ADO", "scope": "platform/Gateway"},
    "jira": {"label": "Jira", "tag": "JIRA", "scope": "PLAT"},
}


def klass(title):
    t = title.lower()
    for words, c in [(("doc", "typo", "spelling", "comment", "link the", "rename"), "docs"),
                     (("test", "flak", "clippy"), "test"),
                     (("slow", "chatty", "twice", "fsync", "latency"), "perf"),
                     (("migrate", "clean", "refactor"), "refactor")]:
        if any(w in t for w in words):
            return c
    return "bug"


def handoff_for(key):
    n = sum(map(ord, key))
    changed = ["Guards the empty case before the loop", "Moves the check ahead of the write", "Caps the value and returns the typed error",
               "Reads the version under the lock", "Replaces the fixed sleep with a wait on the event"]
    verified = ["New test fails before the change and passes after", "Ran the storage suite, all green", "Reproduced it first, then ran the affected tests",
                "Clippy and format clean, targeted tests pass"]
    unsure = ["Not sure the limit should apply to existing streams", "Did not run the cluster tests", "Another caller may rely on the old order", ""]
    return {"changed": changed[n % 5], "verified": verified[n % 4], "unsure": unsure[n % 4]}


class Log:
    def __init__(self):
        self.items = {}
        self.events = []

    def ev(self, ago, kind, item, **d):
        self.events.append({"t": NOW - round(ago), "kind": kind, "item": item, **d})

    def item(self, key, source, ref, title, ago, cls=None):
        self.items[key] = {"source": source, "ref": ref, "title": title, "class": cls or klass(title)}
        self.ev(ago, "imported", key)
        return key

    def propose(self, key, ago, tier, on=(), why="", dup=None):
        d = {"tier": tier, "on": list(on), "why": why}
        if dup:
            d["dup"] = dup
        self.ev(ago, "proposed", key, **d)

    def triage(self, key, ago, tier, on=(), by=None):
        extra = {"by": by} if by else {}
        self.ev(ago, "triaged", key, tier=tier, **extra)
        for d in on:
            self.ev(ago, "linked", key, on=d)

    def ask(self, key, ago, text):
        self.ev(ago, "asked", key, text=text)

    def claim(self, key, ago, agent):
        self.ev(ago, "claimed", key, agent=agent, run="r-" + key.replace("-", "")[:6])

    def fail(self, key, ago, agent, attempts):
        self.ev(ago, "failed", key, agent=agent, attempts=attempts)

    def give_up(self, key, ago, agent, reason, cause="gave up", msg=""):
        self.ev(ago, "gave_up", key, agent=agent, reason=reason, cause=cause, msg=msg)

    def release(self, key, ago, agent):
        self.ev(ago, "released", key, agent=agent)

    def done(self, key, ago, agent, pr, edited=False):
        self.ev(ago, "done", key, agent=agent, pr=pr, **({"edited": True} if edited else {}))

    def bot(self, key, ago):
        self.ev(ago, "bot_review", key, verdict="approve")

    def pr(self, key, ago, agent, pr, add, dele, files, ci, reviewer, handoff=None):
        self.ev(ago, "pr_opened", key, agent=agent, pr=pr, add=add, **{"del": dele}, files=files, ci=ci, reviewer=reviewer,
                handoff=handoff or handoff_for(key))

    def pr_updated(self, key, ago, ci="running"):
        self.ev(ago, "pr_updated", key, ci=ci)

    def ci(self, key, ago, state):
        self.ev(ago, "ci", key, state=state)

    def changes(self, key, ago, by):
        self.ev(ago, "changes_requested", key, by=by)

    def approve(self, key, ago, by):
        self.ev(ago, "approved", key, by=by)

    def reject(self, key, ago, by, reason):
        self.ev(ago, "rejected", key, by=by, reason=reason)

    def answer(self, key, ago, by, text):
        self.ev(ago, "answered", key, by=by, text=text)

    def retry(self, key, ago, by):
        self.ev(ago, "retried", key, by=by)

    def revert(self, key, ago):
        self.ev(ago, "reverted", key)

    def cost(self, key, ago, usd, failed=False):
        self.ev(ago, "cost", key, usd=round(usd, 2), **({"failed": True} if failed else {}))

    def human(self, key, ago, minutes):
        self.ev(ago, "human", key, min=minutes)

    def write(self, name, title, scope, agents, cap=REVIEW_CAP):
        self.events.sort(key=lambda e: e["t"])
        records = [{"o": o, **e} for o, e in enumerate(self.events)]
        doc = {
            "title": title,
            "scope": scope,
            "now": NOW,
            "viewer": VIEWER,
            "people": PEOPLE,
            "settings": {"reviewCap": cap},
            "sources": SOURCES,
            "agents": agents,
            "items": self.items,
        }
        head = json.dumps(doc, indent=1)[:-2]
        body = ",\n".join("  " + json.dumps(r, separators=(",", ":")) for r in records)
        (HERE / name).write_text(f'{head},\n "records": [\n{body}\n ]\n}}\n')
        print(name, len(self.items), "items,", len(records), "records")


def lognorm(r, median, sigma=0.8):
    return median * r.lognormvariate(0, sigma)


CLASS_REJECT = {"perf": 0.30, "refactor": 0.22, "build": 0.18, "bug": 0.12, "test": 0.08, "docs": 0.05}
CLASS_COST = {"perf": 2.4, "refactor": 1.9, "build": 1.2, "bug": 1.4, "test": 0.9, "docs": 0.5}
CLASS_SIZE = {"perf": 90, "refactor": 160, "build": 40, "bug": 60, "test": 35, "docs": 14}
AREAS = ["storage", "gateway", "client", "queue", "replication", "docs", "metrics", "cli", "auth"]
VERBS = ["Fix", "Check", "Handle", "Document", "Retry", "Validate", "Clean up", "Trace", "Speed up"]
OBJS = ["offset gap", "empty batch", "stale lease", "token refresh", "segment roll", "ack timeout",
        "config default", "error text", "reconnect", "fsync mode", "group member", "cache version"]


def history(L, agents):
    """Two weeks of finished work: arrival to merge, with the attempts, review
    rounds, costs, human minutes, rejects and reverts the metrics tab reads."""
    r = random.Random(2024)
    srcs = [("github", "#", "gh", 500), ("ado", "AB#", "ado", 4300), ("jira", "PLAT-", "jira", 100)]
    classes = ["bug", "bug", "bug", "test", "docs", "docs", "perf", "refactor", "build"]
    done = []  # (key, merged_at) for dependencies
    made = 0
    n = 0
    while made < 74:
        n += 1
        src, pre, short, base = srcs[n % 3]
        num = base + n
        key = f"{short}-{num}"
        cls = r.choice(classes)
        title = f"{r.choice(VERBS)} {r.choice(OBJS)} in {r.choice(AREAS)}"
        t = NOW - r.uniform(1.6 * DAY, 13.8 * DAY)
        ev = []  # collected so a lifecycle that runs past the window is dropped whole

        def e(kind, at, **d):
            ev.append({"t": round(at), "kind": kind, "item": key, **d})

        e("imported", t)
        t += lognorm(r, 25 * MIN, 0.5)
        tier = r.choices([0, 1, 2, 3], [1, 3, 5, 3])[0]
        dep = None
        if done and r.random() < 0.16:
            dep = r.choice(done[-12:])
        e("proposed", t, tier=tier, on=[dep[0]] if dep else [], why="")
        reviewer = r.choice(PEOPLE)
        t += min(lognorm(r, 3.2 * HOUR, 0.9), 40 * HOUR)
        if r.random() < 0.07:
            e("rejected", t, by=VIEWER, reason=r.choice(["duplicate", "not a bug", "out of scope"]))
            e("human", t, min=2)
            ev.sort(key=lambda x: x["t"])
            if ev[-1]["t"] < NOW - 3 * HOUR:
                L.items[key] = {"source": src, "ref": f"{pre}{num}", "title": title, "class": cls}
                L.events.extend(ev)
                made += 1
            continue
        e("triaged", t, tier=tier, by=VIEWER)
        if dep:
            e("linked", t, on=dep[0])
        e("human", t, min=r.randint(1, 3))
        ready_at = max(t, dep[1] if dep else 0)
        t = ready_at + min(lognorm(r, 4 * HOUR, 0.9), 30 * HOUR)
        attempts = r.choices([1, 2, 3], [70, 22, 8])[0]
        usd = CLASS_COST[cls]
        ag = r.choice(agents)
        for a in range(1, attempts + 1):
            e("claimed", t, agent=ag, run="r-" + key.replace("-", "")[:6])
            run = r.uniform(25 * MIN, 2.5 * HOUR)
            t += run
            last = a == attempts
            if attempts == 3 and last:
                # Attempts ran out: a person retries it once.
                e("failed", t, agent=ag, attempts=a)
                e("cost", t, usd=round(usd * r.uniform(0.5, 1.4), 2), failed=True)
                e("gave_up", t + 5, agent=ag, reason="attempts used up", cause=r.choice(["tests red", "timeout", "claim lost", "gave up"]), msg="")
                t += min(lognorm(r, 4 * HOUR, 0.8), 24 * HOUR)
                e("retried", t, by=VIEWER)
                e("human", t, min=r.randint(3, 9))
                t += 20 * MIN
                ag = r.choice(agents)
                e("claimed", t, agent=ag, run="r-" + key.replace("-", "")[:6])
                t += r.uniform(30 * MIN, 2 * HOUR)
                break
            if not last:
                e("failed", t, agent=ag, attempts=a)
                e("cost", t, usd=round(usd * r.uniform(0.4, 1.2), 2), failed=True)
                t += 20 * MIN
                ag = r.choice(agents)
        e("cost", t, usd=round(usd * r.uniform(0.6, 1.5), 2))
        size = max(3, int(lognorm(r, CLASS_SIZE[cls], 0.7)))
        files = max(1, size // 22 + r.randint(0, 2))
        e("pr_opened", t, agent=ag, handoff=handoff_for(key), pr=3000 + n, add=size, **{"del": int(size * r.uniform(0.1, 0.6))}, files=files, ci="pass", reviewer=reviewer)
        rounds = r.choices([0, 1, 2], [55, 33, 12])[0]
        for _ in range(rounds):
            t += min(lognorm(r, 9 * HOUR, 0.9), 36 * HOUR)
            e("changes_requested", t, by=reviewer)
            e("human", t, min=r.randint(6, 25))
            t += r.uniform(30 * MIN, 2 * HOUR)
            e("pr_updated", t, ci="pass")
            e("cost", t, usd=round(usd * r.uniform(0.1, 0.4), 2))
        t += min(lognorm(r, 9 * HOUR, 0.9), 40 * HOUR)
        merged = r.random() > CLASS_REJECT[cls]
        if not merged:
            e("rejected", t, by=reviewer, reason=r.choice(["wrong approach", "too broad", "superseded"]))
            e("human", t, min=r.randint(8, 25))
        else:
            e("approved", t, by=reviewer)
            e("human", t, min=r.randint(6, 30))
            t += min(lognorm(r, 1.5 * HOUR, 0.8), 12 * HOUR)
            if r.random() < 0.4:
                e("bot_review", t - 2 * HOUR, verdict="approve")
            e("done", t, agent=ag, pr=3000 + n, **({"edited": True} if r.random() < 0.18 else {}))
            if r.random() < 0.05:
                rv = t + r.uniform(1 * DAY, 6 * DAY)
                if rv < NOW - HOUR:
                    e("reverted", rv)
        ev.sort(key=lambda x: x["t"])
        if max(x["t"] for x in ev if x["kind"] != "reverted") > NOW - 26 * HOUR:
            continue  # recent finished work is hand-written below
        L.items[key] = {"source": src, "ref": f"{pre}{num}", "title": title, "class": cls}
        L.events.extend(ev)
        made += 1
        if merged:
            done.append((key, t))


def hand(capped=False):
    L = Log()
    agents = [f"w{i}" for i in range(1, 7)]
    history(L, agents)
    gh = lambda n, title, ago: L.item(f"gh-{n}", "github", f"#{n}", title, ago)
    ado = lambda n, title, ago: L.item(f"ado-{n}", "ado", f"AB#{n}", title, ago)
    jira = lambda n, title, ago: L.item(f"jira-{n}", "jira", f"PLAT-{n}", title, ago)

    # Triage: waiting for a person to accept the triage agent's proposal. The
    # newest has not been read yet.
    gh(812, "Panic on an empty publish batch through the gateway", 2 * MIN)
    gh(811, "Subscribe with a zero credit window never delivers", 5 * MIN)
    ado(4410, "Gateway rejects tokens after a signing key rotation", 21 * MIN)
    jira(229, "Consumer group refusal message does not name the group", 48 * MIN)
    gh(809, "Docs: the retention page still shows the old default", 2 * HOUR + 10 * MIN)
    gh(808, "Cache put_if returns the wrong version on a tied write", 5 * HOUR)
    ado(4408, "Dashboard shows negative lag after a reset", 9 * HOUR)
    L.propose("gh-811", 4 * MIN, 1, why="Hangs a subscriber; same path as the credit window fix", dup="gh-774")
    L.propose("ado-4410", 15 * MIN, 1, ["ado-4395"], "Touches the token refresh that AB#4395 changes")
    L.propose("jira-229", 40 * MIN, 3, why="Message text only")
    L.propose("gh-809", 2 * HOUR, 3, why="Docs page, no code")
    L.propose("gh-808", 4 * HOUR + 40 * MIN, 1, why="Wrong version breaks put_if retries")
    L.propose("ado-4408", 8 * HOUR + 30 * MIN, 2, why="Display only, reset is rare", dup="gh-765")

    # The held item the chain waits behind.
    k = gh(798, "Group state is lost on a leader change", 2 * DAY + 3 * HOUR)
    L.triage(k, 2 * DAY + 2 * HOUR, 0)
    L.claim(k, 5 * HOUR, "w2")
    for n, t, tier, on, ago in [
        (801, "Regression test for group state across a leader change", 1, ["gh-798"], 2 * DAY),
        (799, "Document group recovery in durable-storage", 2, ["gh-798"], 2 * DAY + HOUR),
        (803, "Metric for group recovery time", 2, ["gh-799"], 30 * HOUR),
    ]:
        k = gh(n, t, ago + HOUR)
        L.triage(k, ago, tier, on)
    k = jira(218, "Customer asks when group recovery is safe to rely on", 40 * HOUR)
    L.triage(k, 38 * HOUR, 1, ["gh-801"])
    k = ado(4399, "Migrate the gateway client to the new group API", 3 * DAY)
    L.triage(k, 3 * DAY - HOUR, 1, ["ado-4402"])

    # Needs you.
    k = gh(790, "Retention sweep skips a sealed segment after restart", 2 * DAY)
    L.triage(k, 2 * DAY - 20 * MIN, 0)
    L.ask(k, 2 * DAY - 40 * MIN, "Does it reproduce on 0.7.0, or only on main?")
    k = gh(774, "Cache TTL test fails one run in twenty", 3 * DAY)
    L.triage(k, 3 * DAY - 10 * MIN, 2)
    L.claim(k, 20 * HOUR, "w3")
    L.fail(k, 19 * HOUR, "w3", 1)
    L.claim(k, 18 * HOUR, "w1")
    L.fail(k, 17 * HOUR, "w1", 2)
    L.give_up(k, 17 * HOUR - 5, "w1", "attempts used up", "tests red", "The test still fails 1 run in 20; I cannot tell whether the TTL or the clock is wrong.")
    k = ado(4377, "Duplicate delivery reported by one tenant", 26 * HOUR)
    L.triage(k, 25 * HOUR, 1)
    L.ask(k, 22 * HOUR, "Which tenant, and can we see their consumer logs?")
    k = jira(214, "Intermittent timeout on the first publish after idle", 5 * HOUR)
    L.triage(k, 4 * HOUR, 2)
    L.claim(k, 3 * HOUR, "w4")
    L.give_up(k, 2 * HOUR, "w4", "cannot reproduce", "gave up", "Ran the first publish after idle 200 times and never saw the timeout.")

    # Held by an agent.
    for key, n, title, tier, ago, claimed, agent in [
        ("gh", 805, "Wrong error code on a stale fence", 1, 30 * HOUR, 40 * MIN, "w5"),
        ("ado", 4388, "Retry loop ignores the server's backoff hint", 1, 2 * DAY, 3 * HOUR, "w4"),
        ("jira", 224, "Typo in the connection error text", 3, 20 * HOUR, 12 * MIN, "w6"),
    ]:
        k = {"gh": gh, "ado": ado, "jira": jira}[key](n, title, ago + HOUR)
        L.triage(k, ago, tier)
        L.claim(k, claimed, agent)

    # Ready, by tier.
    ready = [
        ("gh", 780, "Reject a publish larger than the stream maximum", 0, 9 * HOUR),
        ("gh", 781, "Leader lease renewal races with shutdown", 0, 11 * HOUR),
        ("gh", 702, "Subscriber queue overflow drops without a counter", 1, 3 * DAY),
        ("gh", 755, "Offsets repeat after a torn segment repair", 1, 40 * HOUR),
        ("gh", 761, "Sparse index rebuild reads the segment twice", 1, 33 * HOUR),
        ("ado", 4402, "Client reconnect storm after a broker restart", 1, 2 * DAY + 5 * HOUR),
        ("ado", 4395, "Token refresh races with a long poll", 1, 30 * HOUR),
        ("jira", 231, "Kafka group refusal needs a doc link", 2, 18 * HOUR),
        ("gh", 740, "List streams is slow with ten thousand streams", 2, 4 * DAY - 3 * HOUR),
        ("gh", 744, "Console log of a replica catch-up is too chatty", 2, 2 * DAY),
        ("gh", 748, "Make the fsync mode visible in the stats call", 2, 26 * HOUR),
        ("gh", 752, "TypeScript client: unclear error on a closed stream", 2, 22 * HOUR),
        ("ado", 4391, "Gateway docs show a removed header", 2, 19 * HOUR),
        ("ado", 4389, "Add a health check for the token cache", 2, 15 * HOUR),
        ("jira", 226, "Rename a metric to match the docs", 2, 9 * HOUR),
        ("gh", 733, "Spelling in the storage format page", 3, 3 * DAY),
        ("gh", 735, "Example config comments are out of date", 3, 2 * DAY + 4 * HOUR),
        ("jira", 210, "Add the broker version to the startup banner", 3, 2 * DAY),
        ("jira", 221, "Link the failover page from the quick start", 3, 27 * HOUR),
    ]
    mk = {"gh": gh, "ado": ado, "jira": jira}
    for src, n, title, tier, ago in ready:
        k = mk[src](n, title, ago + 30 * MIN)
        L.triage(k, ago, tier)
    # A claim that lapsed: the item went back to the queue.
    L.claim("gh-755", 38 * HOUR, "w6")
    L.release("gh-755", 37 * HOUR, "w6")

    # In review: agent PRs waiting on a person, a CI result or the agent.
    def review(src, n, title, tier, ago, claimed, agent, pr, add, dele, files, ci, reviewer, rounds=0, approved=False):
        k = mk[src](n, title, ago + HOUR)
        L.triage(k, ago, tier)
        L.claim(k, claimed, agent)
        L.cost(k, claimed - 5 * MIN, 1.1 + tier * 0.2)
        L.pr(k, claimed - 25 * MIN, agent, pr, add, dele, files, ci if rounds == 0 else "pass", reviewer)
        for i in range(rounds):
            L.changes(k, claimed - 3 * HOUR - i * 4 * HOUR, reviewer)
            L.human(k, claimed - 3 * HOUR - i * 4 * HOUR, 12)
            L.pr_updated(k, claimed - 3 * HOUR - i * 4 * HOUR - 90 * MIN, "pass")
        if rounds:
            L.ci(k, claimed - 20 * MIN, ci)
        if approved:
            L.approve(k, claimed - 60 * MIN, reviewer)
        return k

    # Oldest first: the footer names this stage when it is the slowest.
    review("gh", 789, "Validate the heartbeat interval upper bound", 2, 3 * DAY, 31 * HOUR, "w2", 3118, 64, 12, 3, "pass", "dana")
    review("ado", 4385, "Return 404 for a deleted tenant's stream", 1, 2 * DAY, 14 * HOUR, "w4", 3121, 212, 40, 6, "running", "kim")
    review("gh", 776, "Fix the flaky sparse index test", 2, 2 * DAY, 9 * HOUR, "w3", 3123, 18, 9, 1, "pass", "dana", rounds=2)
    review("jira", 219, "Clarify the retention warning text", 3, 30 * HOUR, 2 * HOUR, "w5", 3124, 6, 6, 1, "fail", "ravi")
    review("ado", 4383, "Lease renewal logs the wrong node", 2, 2 * DAY, 7 * HOUR, "w6", 3127, 24, 8, 2, "pass", "kim", approved=True)
    L.bot("gh-789", 30 * HOUR)
    if capped:
        review("gh", 771, "Reject duplicate consumer ids", 1, 2 * DAY, 20 * HOUR, "w1", 3126, 150, 22, 5, "pass", None)

    # Done in the last day, and some older.
    for i, (src, n, title, ago, done, agent) in enumerate([
        ("gh", 801 + 1, "Stale docs link in the queue page", 3 * DAY, 20 * HOUR, "w2"),
        ("gh", 786, "Segment roll-over test uses a fixed sleep", 3 * DAY, 15 * HOUR, "w1"),
        ("gh", 784, "Fix typo in the CLI help for retention", 3 * DAY, 14 * HOUR, "w3"),
        ("gh", 779, "Clippy: needless clone in the subscribe path", 3 * DAY, 11 * HOUR, "w5"),
        ("gh", 777, "Group poll returns early on an empty batch", 3 * DAY, 8 * HOUR, "w2"),
        ("gh", 770, "Cap the retry delay in the Rust client", 3 * DAY, 6 * HOUR, "w4"),
        ("ado", 4381, "Gateway returns 500 for an unknown stream", 3 * DAY, 5 * HOUR, "w6"),
        ("ado", 4375, "Missing content type on an error body", 3 * DAY, 3 * HOUR, "w1"),
        ("jira", 205, "Wrong unit in the latency chart label", 3 * DAY, 100 * MIN, "w3"),
        ("jira", 202, "Document the offset type in the client page", 3 * DAY, 55 * MIN, "w5"),
        ("gh", 765, "Consumer lag metric is off by one", 3 * DAY, 2 * DAY, "w4"),
        ("gh", 760, "Heartbeat interval is not validated", 3 * DAY, 2 * DAY + 4 * HOUR, "w2"),
    ]):
        k = mk[src](n, title, ago + HOUR)
        rv = [3, 6, 2, 9, 4, 12, 1, 5, 2, 7, 8, 3][i] * HOUR
        L.triage(k, ago, 2 if n % 2 else 3)
        L.claim(k, done + rv + 40 * MIN, agent)
        L.cost(k, done + rv, 0.6 + (i % 5) * 0.35)
        L.pr(k, done + rv, agent, 3000 + n, 10 + i * 9, 3 + i, 1 + i % 4, "pass", PEOPLE[i % 3])
        L.human(k, done + 30 * MIN, 8 + i)
        L.done(k, done, agent, 3000 + n, edited=i % 5 == 2)

    name = "fixture-work-capped.json" if capped else "fixture-work.json"
    L.write(name, "Review limit reached" if capped else "Morning intake", "GetFelix/felix, platform/Gateway, PLAT", agents)
    return L


def burst():
    r = random.Random(77)
    L = Log()
    # Three imports landed within the last hour; the rest of the board is older.
    sources = [("github", 190, "#", "gh"), ("ado", 70, "AB#", "ado"), ("jira", 60, "PLAT-", "jira")]
    stages = ["arrived"] * 165 + ["ready"] * 66 + ["blocked"] * 34 + ["held"] * 12 + ["needs"] * 9 + ["review"] * 14 + ["done"] * 20
    r.shuffle(stages)
    agents = [f"w{i}" for i in range(1, 13)]
    roots, blocked = [], []
    n = 0
    for src, count, pre, short in sources:
        for _ in range(count):
            stage = stages[n]
            n += 1
            key = f"{short}-{1000 + n}"
            title = f"{r.choice(VERBS)} {r.choice(OBJS)} in {r.choice(AREAS)}"
            if stage == "arrived":
                ago = r.uniform(2, 55) * MIN if r.random() < 0.85 else r.uniform(1, 30) * HOUR
                L.item(key, src, f"{pre}{1000 + n}", title, ago)
                if ago > 8 * MIN and r.random() < 0.8:
                    L.propose(key, ago - 5 * MIN, r.choices([0, 1, 2, 3], [1, 3, 5, 3])[0], why="Read by the triage agent")
                continue
            ago = r.uniform(3, 70) * HOUR
            L.item(key, src, f"{pre}{1000 + n}", title, ago)
            tri = ago - r.uniform(5, 30) * MIN
            tier = r.choices([0, 1, 2, 3], [1, 3, 5, 3])[0]
            deps = []
            if stage == "blocked":
                pool = roots + blocked
                deps = [r.choice(pool)] if pool else []
            L.triage(key, tri, tier, deps)
            if stage == "blocked" and deps:
                blocked.append(key)
            elif stage == "blocked":
                stage = "ready"
            if stage == "needs":
                L.ask(key, tri - 20 * MIN, "Which environment does this happen in?")
            elif stage == "held":
                L.claim(key, r.uniform(1, 30) * MIN, agents[len(roots)])
                roots.append(key)
            elif stage == "review":
                ag = r.choice(agents)
                claim = r.uniform(2, 40) * HOUR
                L.claim(key, claim, ag)
                L.pr(key, claim - r.uniform(20, 90) * MIN, ag, 5000 + n, r.randint(5, 300), r.randint(1, 80), r.randint(1, 9),
                     r.choice(["pass", "pass", "running", "fail"]), r.choice(PEOPLE))
            elif stage == "done":
                ag = r.choice(agents)  # finished work, so the agent is free again
                claim = r.uniform(1, 20) * HOUR
                L.claim(key, claim, ag)
                L.done(key, claim - r.uniform(10, 40) * MIN, ag, 4000 + n)
    L.write("fixture-work-burst.json", "Burst intake", "an import of the old backlog", agents, cap=20)
    return L


hand()
hand(capped=True)
burst()
