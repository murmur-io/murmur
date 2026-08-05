# wkwebview-probe

Run JavaScript inside the **real** WKWebView Murmur ships in, from the command line.

```bash
swiftc -O -o /tmp/wkprobe scripts/wkwebview-probe/main.swift

# is an API actually available in the engine we ship?
/tmp/wkprobe --url http://localhost:1420/ \
  --eval 'return { colorMix: CSS.supports("color","color-mix(in srgb, red, blue)"),
                   has: CSS.supports("selector(:has(*))") }'

# does a page render without throwing?
/tmp/wkprobe --url http://localhost:1420/dashboards --settle 3 \
  --eval-file probe.js --timeout 30
```

Output is a single JSON object on stdout (`{"value": …}`); diagnostics go to stderr.
Exit codes: `0` ok · `1` bad args · `2` navigation failed · `3` the expression threw · `4` timeout.

## Why this exists

The e2e suite runs in Playwright's Chromium and WebKit. Those are **newer** than the WKWebView on a
given macOS, so a modern web API can pass every gate and still fail for the user — and nothing in
this repo could execute a line of JS in the engine that actually ships.

That gap cost six rounds on 2026-08-04 (PR #566/#568). The palette "did not open"; three of the
fixes were built on the hypothesis that `matches(":modal")` was throwing on the shipping engine.
Nobody could check. The first thing this probe did, once it existed, was **falsify that hypothesis
in ten seconds**:

```
matches(":modal") threw: null      # it does not throw
showModal() threw:     null        # it is not refused
:modal after showModal: true       # it works correctly
```

The real cause was the wire contract (snake_case payload vs camelCase FE). See
`.claude/rules/angular-zoneless.md` T5 (corrected) and T6.

## What it does and does not prove

**Proves:** whether a JS/CSS API exists and behaves in the shipping engine; whether a page
bootstraps and renders there without throwing.

**Does not prove:** anything requiring Tauri itself — `window.__TAURI_INTERNALS__` is absent, so the
app runs without IPC. Use it for engine-capability and render questions, not for data flows. It also
says nothing about a signed build, Touch ID, ScreenCaptureKit or the lock model; those still need a
real notarized run on a real Mac.

## Notes

- No accessibility grant needed. Driving the packaged app with AppleScript requires one, and the
  permission dialog **hangs a non-interactive shell** — the same trap as the `security` CLI.
- Headless: the web view is never attached to a window.
- `--settle` (default 2s) is the pause after load before evaluating; an Angular app needs a beat to
  bootstrap or the probe reads an empty `<app-root>`.
