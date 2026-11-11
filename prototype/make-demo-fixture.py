#!/usr/bin/env python3
"""Writes fixture-work-demo.json, the records behind the autoplay demo (?demo=1).

The page starts from `records`, a quiet board, and plays `beats` into the log
one after another. Every beat is a list of records taken from the decision
replay fixture's gh-792 history (retargeted to a new item) or written the way
that fixture writes them. Nothing is generated at play time. Offsets that point
at another record are written as "@id" and resolved when the record is added.
Same input, same file.
"""
import json
from pathlib import Path

HERE = Path(__file__).parent
HOUR = 3600
BASE = 14 * 86400
SRC = json.load(open(HERE / "fixture-work-replay.json"))
STORY = "gh-801"

items = {k: v for k, v in SRC["items"].items() if k in ("gh-792",)}
items = {
    "gh-780": {"source": "github", "ref": "#780", "title": "Retention sweep skips the newest segment", "class": "bug"},
    "ado-4388": {"source": "ado", "ref": "AB#4388", "title": "Lease renewal logs the wrong holder", "class": "bug"},
    "jira-209": {"source": "jira", "ref": "PLAT-209", "title": "Quickstart lists a removed flag", "class": "docs"},
    "gh-796": {"source": "github", "ref": "#796", "title": "Idle publisher is dropped after 30 s", "class": "bug"},
    "gh-798": {"source": "github", "ref": "#798", "title": "Metrics endpoint reports negative lag", "class": "bug"},
    "ado-4402": {"source": "ado", "ref": "AB#4402", "title": "Gateway returns 500 on an empty batch", "class": "bug"},
    STORY: {"source": "github", "ref": "#801", "title": "Subscribe resumes one record early after a reconnect", "class": "bug"},
    "ado-4420": {"source": "ado", "ref": "AB#4420", "title": "Retry drops the idempotency key", "class": "bug"},
    "jira-231": {"source": "jira", "ref": "PLAT-231", "title": "Retention example uses the removed flag", "class": "docs"},
}

records = []
def add(t, kind, item, **kw):
    records.append({"o": len(records), "t": BASE + t, "kind": kind, "item": item, **kw})

def done(key, ago, agent, pr, tier):
    add(-ago - 5 * HOUR, "imported", key); add(-ago - 4 * HOUR, "triaged", key, tier=tier, by="dana")
    add(-ago - 3 * HOUR, "claimed", key, agent=agent, run="r-" + key)
    add(-ago - 2 * HOUR, "pr_opened", key, agent=agent, pr=pr, add=14, **{"del": 3}, files=2, ci="pass", reviewer="kim")
    add(-ago - HOUR, "approved", key, by="kim"); add(-ago, "done", key, agent=agent, pr=pr)

done("gh-780", 3 * HOUR, "w2", 3098, 1)
done("ado-4388", 6 * HOUR, "w4", 3101, 2)
done("jira-209", 11 * HOUR, "w5", 3103, 3)
add(-5 * HOUR, "imported", "gh-796"); add(-4 * HOUR, "triaged", "gh-796", tier=2, by="dana")
for key, agent, ago in (("gh-798", "w1", 40 * 60),):
    add(-ago - 2 * HOUR, "imported", key); add(-ago - HOUR, "triaged", key, tier=2, by="dana")
    add(-ago, "claimed", key, agent=agent, run="r-" + key)
add(-3 * HOUR, "imported", "ado-4402"); add(-150 * 60, "triaged", "ado-4402", tier=1, by="dana")
add(-130 * 60, "claimed", "ado-4402", agent="w3", run="r-ado-4402")
add(-60 * 60, "pr_opened", "ado-4402", agent="w3", pr=3112, add=9, **{"del": 2}, files=2, ci="pass", reviewer="kim",
    handoff={"changed": "An empty batch returns 204", "verified": "The batch tests pass", "unsure": ""})
records.sort(key=lambda r: r["t"])
for i, r in enumerate(records): r["o"] = i

def arrive(key, tier, why, proposer="w6"):
    return [
        {"dt": 20, "kind": "imported", "item": key},
        {"dt": 30, "kind": "proposed", "item": key, "tier": tier, "on": [], "why": why, "agent": proposer},
    ]

story = [r for r in SRC["records"] if r["item"] == "gh-792" and r["o"] >= 1106]
a1 = [r for r in story if r.get("attempt") in (1, None) and r["o"] < 1200]
a2 = [r for r in story if r["o"] >= 1217 and r["o"] <= 1229]
ids = {r["o"]: f"s{r['o']}" for r in SRC["records"]}
ids[1212] = "redo"

def ref(v):
    if isinstance(v, bool) or v is None: return v
    if isinstance(v, int): return "@" + ids[v]
    return v

def retarget(rs, claim=False):
    out, prev = [], None
    for r in rs:
        r = json.loads(json.dumps(r)); o = r.pop("o"); t = r.pop("t")
        r["item"] = STORY; r["id"] = ids[o] if r["kind"] in ("decision", "tool", "edit") else r.get("id")
        r["dt"] = 30 if prev is None else t - prev; prev = t
        if r["kind"] in ("decision",):
            r["cites"] = [ref(c) for c in r["cites"]]
            for s in r.get("so_far", []): s["cites"] = [ref(c) for c in s["cites"]]
        if r["kind"] == "edit": r["decision"] = ref(r["decision"]) if r.get("decision") else None
        if r["kind"] == "pr_opened": r["reviewer"] = "dana"
        if r.get("id") is None: r.pop("id")
        out.append(r)
    return out

a1r = retarget(a1)
claim1 = [a1r[0]]
work1 = [r for r in a1r[1:] if r["kind"] != "pr_opened"]
pr1 = [r for r in a1r if r["kind"] == "pr_opened"]
a2r = retarget(a2)
work2 = [r for r in a2r if r["kind"] != "pr_opened"]
pr2 = [r for r in a2r if r["kind"] == "pr_opened"]
for r in work2 + pr2: r["attempt"] = 2

beats = {
    "arrive_gh": arrive(STORY, 1, "Duplicate delivery after a reconnect; every durable subscriber can hit it."),
    "arrive_ado": arrive("ado-4420", 2, "A retry resends the body without its key, so a charge can repeat."),
    "arrive_jira": arrive("jira-231", 3, "Docs only; the flag was removed two releases ago."),
    "claim_gh": claim1,
    "claim_ado": [{"dt": 20, "kind": "claimed", "item": "ado-4420", "agent": "w2", "run": "r-ado-4420a"}],
    "lapse_ado": [{"dt": 900, "kind": "released", "item": "ado-4420", "agent": "w2"}],
    "reclaim_ado": [{"dt": 20, "kind": "claimed", "item": "ado-4420", "agent": "w4", "run": "r-ado-4420b"}],
    "claim_jira": [{"dt": 20, "kind": "claimed", "item": "jira-231", "agent": "w6", "run": "r-jira-231"}],
    "work_gh": work1,
    "pr_gh": pr1,
    "pr_ado": [{"dt": 600, "kind": "pr_opened", "item": "ado-4420", "agent": "w4", "pr": 3129, "add": 11, "del": 2, "files": 2, "ci": "pass",
                "reviewer": "kim", "handoff": {"changed": "A retry carries the original idempotency key", "verified": "The retry tests pass", "unsure": ""}}],
    "work_gh2": work2,
    "pr_gh2": pr2,
}
redo = {"instruction": next(r["text"] for r in SRC["records"] if r["o"] == 1212)}
out = {
    "redo": redo,
    "title": "Autoplay demo", "scope": SRC["scope"], "now": BASE, "viewer": "dana", "people": SRC["people"],
    "settings": {"reviewCap": 8}, "sources": SRC["sources"], "agents": SRC["agents"],
    "items": items, "records": records, "beats": beats,
}
(HERE / "fixture-work-demo.json").write_text(json.dumps(out, indent=1, ensure_ascii=False) + "\n")
print("wrote fixture-work-demo.json:", len(records), "records,", {k: len(v) for k, v in beats.items()})
