Known secret shapes (GitHub, GitLab, Slack, Google, npm and Stripe tokens, model provider keys, AWS access key IDs,
JWTs, `Bearer` and `Basic` credentials, `token=` values, URL passwords and PEM private keys) are replaced with
`[redacted]` in the free text of every record an agent writes and of a person's note, by the tracker before the
append and by `kbl-mcp` before it stores long text, a task result, a task's title or reason, or the error text of a `refused` run log record. Secrets with no
prefix, such as AWS secret access keys, are not caught.
