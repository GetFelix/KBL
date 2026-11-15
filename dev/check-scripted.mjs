// A scripted run is reproducible from its seed: the same seed run twice, by
// different workers in a different order, ends every task the same way, and
// that way is the one the seed promises.
//
//   cargo build -p kbl-mcp -p kbl-tool && node dev/check-scripted.mjs
import { expected, plan, sameEnds } from "./lib/script.mjs";
import { check, felixToken, giveUpAfter, killAll } from "./lib/stack.mjs";
import { scriptedRun } from "./scripted.mjs";

giveUpAfter(240, "the scripted check");
try {
  const token = await felixToken();
  const seed = 2;
  const first = await scriptedRun({ seed, workers: 3, token });
  const fates = new Set(first.tasks.map((t) => t.fate));
  check(fates.size === 4, `seed ${seed} gives ${first.tasks.length} tasks that cover every fate`);
  check(sameEnds(first.ends, expected(first.tasks)), "every task ended as the seed says");
  const second = await scriptedRun({ seed, workers: 2, token });
  check(sameEnds(second.ends, first.ends), "the same seed with other workers ended every task the same way");
  check(JSON.stringify(plan(seed)) !== JSON.stringify(plan(seed + 1)), "another seed plans another job");
} finally {
  killAll();
}
