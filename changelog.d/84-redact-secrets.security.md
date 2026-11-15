Known secret shapes (GitHub, GitLab, Slack and Google tokens, model provider keys, AWS access key IDs, `Bearer`
tokens and PEM private keys) are replaced with `[redacted]` in the free text of every record an agent writes, by the
tracker before the append and by `kbl-mcp` before it stores long text as an artifact.
