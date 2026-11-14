An issue reopened after ingest forgot it, while no intake rule matched it, now
takes the first rule that matches it later (if that rule is under its cap) and
comes back into scope. Before, it had no rule and stayed out for good.
