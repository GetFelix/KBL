Confine the fake worker's MCP config path to the supervisor's state dir (regular file, no symlink); remove a worker's config with tokio::fs; drop a backtracking regex in the dashboard check.
