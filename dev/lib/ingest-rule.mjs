// The rule dev/ingest.mjs writes for one repository, built from its flags.
const list = (text) => (text ?? "").split(",").map((l) => l.trim()).filter(Boolean);

/**
 * `labels` is the --labels (or old --label) text, undefined when not given. With no
 * label flag the default is `agent-ok` only when no other filter narrows the rule;
 * an explicit empty --labels means no label filter at all.
 */
export function buildRule({ repo, labels, excludeLabel, milestone, cap, mode = "admit", triage = "run" }) {
  const excluded = list(excludeLabel);
  const labelsAll = labels !== undefined ? list(labels) : excluded.length || milestone ? [] : ["agent-ok"];
  const parts = [];
  if (labelsAll.length) parts.push(labelsAll.join("+"));
  if (excluded.length) parts.push(`without ${excluded.join("+")}`);
  if (milestone) parts.push(`milestone ${milestone}`);
  const rule = { name: parts.join(", ") || repo, repo, mode, labels_all: labelsAll };
  if (triage === "skip") rule.triage = "skip";
  if (excluded.length) rule.labels_excluded = excluded;
  if (milestone) rule.milestone = milestone;
  if (cap !== undefined) rule.cap = cap;
  return rule;
}
