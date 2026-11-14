An imported issue whose rule is no longer in the ingest config (renamed or
removed), or that was imported again after ingest forgot it while no rule
matched, now takes the first rule that matches it. Before, it left scope and
stayed out for good.
