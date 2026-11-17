The tracker prunes snapshot files by the log they belong to, so files left from before a retention trim are removed, and a save that started before a rebuild no longer writes or prunes after it.
