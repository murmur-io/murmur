# `.claude/traces/` — per-task evidence archive

In-flight run artifacts (gate JSONs, `trace.jsonl` spans) live in `.claude/tmp/<task>/`, which is
gitignored scratch. When a task lands and you want a durable, reviewable record, archive that dir
here as `.claude/traces/<task>/` with a one-line `MANIFEST.txt`:

```
task=<slug> outcome=PASS|FAIL branch=<branch> archived_at=<UTC>
```

This is optional and manual today — the machine gate (`finish-guard.sh`) reads from `.claude/tmp/`,
not from here. Archive a trace when a run is worth keeping (a tricky lock change, a release, a bug
whose evidence you want to point back to). Keep archived traces PII-free: IDs, stages, counts,
verdicts — never note/transcript text.
