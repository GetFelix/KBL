#!/usr/bin/env python3
"""Writes the intake view's fixtures.

fixture-work.json is a hand-written morning: three upstream trackers, a
blocked chain behind one held item, items waiting on a person, held items with
their agents, all four tiers. fixture-work-burst.json is generated: a few
hundred items, most of them arriving in the last hour, to check the view stays
readable. Same input, same files.

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
NOW = 4 * DAY  # the fixtures end here; "ago" counts back from it

SOURCES = {
    "github": {"label": "GitHub", "tag": "GH", "scope": "GetFelix/felix"},
    "ado": {"label": "Azure DevOps", "tag": "ADO", "scope": "platform/Gateway"},
    "jira": {"label": "Jira", "tag": "JIRA", "scope": "PLAT"},
}


class Log:
    def __init__(self):
        self.items = {}
        self.events = []

    def ev(self, ago, kind, item, **d):
        self.events.append({"t": NOW - round(ago), "kind": kind, "item": item, **d})

    def item(self, key, source, ref, title, ago):
        self.items[key] = {"source": source, "ref": ref, "title": title}
        self.ev(ago, "imported", key)
        return key

    def triage(self, key, ago, tier, on=()):
        self.ev(ago, "triaged", key, tier=tier)
        for d in on:
            self.ev(ago, "linked", key, on=d)

    def ask(self, key, ago, text):
        self.ev(ago, "asked", key, text=text)

    def claim(self, key, ago, agent):
        self.ev(ago, "claimed", key, agent=agent, run="r-" + key.replace("-", "")[:6])

    def fail(self, key, ago, agent, attempts):
        self.ev(ago, "failed", key, agent=agent, attempts=attempts)

    def give_up(self, key, ago, agent, reason):
        self.ev(ago, "gave_up", key, agent=agent, reason=reason)

    def release(self, key, ago, agent):
        self.ev(ago, "released", key, agent=agent)

    def done(self, key, ago, agent, pr):
        self.ev(ago, "done", key, agent=agent, pr=pr)

    def write(self, name, title, scope, agents):
        self.events.sort(key=lambda e: e["t"])
        records = [{"o": o, **e} for o, e in enumerate(self.events)]
        doc = {
            "title": title,
            "scope": scope,
            "now": NOW,
            "sources": SOURCES,
            "agents": agents,
            "items": self.items,
        }
        head = json.dumps(doc, indent=1)[:-2]
        body = ",\n".join("  " + json.dumps(r, separators=(",", ":")) for r in records)
        (HERE / name).write_text(f'{head},\n "records": [\n{body}\n ]\n}}\n')
        print(name, len(self.items), "items,", len(records), "records")


def hand():
    L = Log()
    gh = lambda n, title, ago: L.item(f"gh-{n}", "github", f"#{n}", title, ago)
    ado = lambda n, title, ago: L.item(f"ado-{n}", "ado", f"AB#{n}", title, ago)
    jira = lambda n, title, ago: L.item(f"jira-{n}", "jira", f"PLAT-{n}", title, ago)

    # Arrived: not triaged yet.
    gh(812, "Panic on an empty publish batch through the gateway", 2 * MIN)
    gh(811, "Subscribe with a zero credit window never delivers", 5 * MIN)
    ado(4410, "Gateway rejects tokens after a signing key rotation", 21 * MIN)
    jira(229, "Consumer group refusal message does not name the group", 48 * MIN)
    gh(809, "Docs: the retention page still shows the old default", 2 * HOUR + 10 * MIN)
    gh(808, "Cache put_if returns the wrong version on a tied write", 5 * HOUR)
    ado(4408, "Dashboard shows negative lag after a reset", 9 * HOUR)

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
    L.give_up(k, 17 * HOUR - 5, "w1", "attempts used up")
    k = ado(4377, "Duplicate delivery reported by one tenant", 26 * HOUR)
    L.triage(k, 25 * HOUR, 1)
    L.ask(k, 22 * HOUR, "Which tenant, and can we see their consumer logs?")
    k = jira(214, "Intermittent timeout on the first publish after idle", 5 * HOUR)
    L.triage(k, 4 * HOUR, 2)
    L.claim(k, 3 * HOUR, "w4")
    L.give_up(k, 2 * HOUR, "w4", "cannot reproduce")

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

    # Done in the last day, and some older.
    for src, n, title, ago, done, agent in [
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
    ]:
        k = mk[src](n, title, ago + HOUR)
        L.triage(k, ago, 2 if n % 2 else 3)
        L.claim(k, done + 25 * MIN, agent)
        L.done(k, done, agent, 3000 + n)

    L.write("fixture-work.json", "Morning intake", "GetFelix/felix, platform/Gateway, PLAT", [f"w{i}" for i in range(1, 7)])
    return L


def burst():
    r = random.Random(77)
    L = Log()
    areas = ["storage", "gateway", "client", "queue", "replication", "docs", "metrics", "cli", "auth"]
    verbs = ["Fix", "Check", "Handle", "Document", "Retry", "Validate", "Clean up", "Trace"]
    objs = ["offset gap", "empty batch", "stale lease", "token refresh", "segment roll", "ack timeout",
            "config default", "error text", "reconnect", "fsync mode", "group member", "cache version"]
    # Three imports landed within the last hour; the rest of the board is older.
    sources = [("github", 190, "#", "gh"), ("ado", 70, "AB#", "ado"), ("jira", 60, "PLAT-", "jira")]
    stages = ["arrived"] * 165 + ["ready"] * 80 + ["blocked"] * 34 + ["held"] * 12 + ["needs"] * 9 + ["done"] * 20
    r.shuffle(stages)
    agents = [f"w{i}" for i in range(1, 13)]
    roots, blocked = [], []
    n = 0
    for src, count, pre, short in sources:
        for _ in range(count):
            stage = stages[n]
            n += 1
            key = f"{short}-{1000 + n}"
            title = f"{r.choice(verbs)} {r.choice(objs)} in {r.choice(areas)}"
            if stage == "arrived":
                ago = r.uniform(2, 55) * MIN if r.random() < 0.85 else r.uniform(1, 30) * HOUR
                L.item(key, src, f"{pre}{1000 + n}", title, ago)
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
            elif stage == "done":
                ag = r.choice(agents)  # finished work, so the agent is free again
                claim = r.uniform(1, 20) * HOUR
                L.claim(key, claim, ag)
                L.done(key, claim - r.uniform(10, 40) * MIN, ag, 4000 + n)
    L.write("fixture-work-burst.json", "Burst intake", "an import of the old backlog", agents)
    return L


hand()
burst()
