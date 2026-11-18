A question frees the agent (format 14). `work_ask` ends the claim without using a try and parks the item in Needs you; a
plain answer sends the item back to Ready with the question and answer in its context. `work_release` takes `why`:
`waits_on_person` and `waits_on_outside` use no try and park the item until a person moves it out; only failures count
toward the two tries. An agent whose claim lapsed while it waited may still ask or release as a wait once. Every
`work_next` reply and the `kbl-mcp` tool descriptions say questions go through `work_ask`, never the host's chat. Raise
the format to 14 after upgrading (docs/upgrades.md); the golden `wasm/golden/format_14.jsonl` and
`wasm/golden/format_14.state.json` pin the new fold.
