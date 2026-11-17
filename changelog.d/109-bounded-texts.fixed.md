The tracker keeps at most 8 MiB of context, note, question and answer text in memory and reads the rest back from the log on demand.
When retention has trimmed the text an ingest context entry would be compared with, ingest writes the current text instead of skipping it.
