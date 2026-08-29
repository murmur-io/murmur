# wkwebview-drag-probe

Does a drag reach the **page** inside the WKWebView Murmur ships in — or does the native layer in
front of it eat the gesture first?

```bash
swiftc -O -o /tmp/dragprobe scripts/wkwebview-drag-probe/main.swift

/tmp/dragprobe               # the comparison table
/tmp/dragprobe --self-test   # exit 1 unless the asymmetry still holds
/tmp/dragprobe --file        # drag a file rather than text
```

```
dragDropEnabled: true  (wry answers, WKWebView never sees it)
  draggingEntered=1 draggingUpdated=1  page saw {"enter":0,"over":0,"drop":0}
dragDropEnabled: false (wry declines, WKWebView handles it)
  draggingEntered=1 draggingUpdated=16 page saw {"enter":1,"over":1,"drop":1}
```

## Why this exists

Sidebar drag-and-drop worked in every gate and in neither shipped build. `e2e/shell/workspace-dnd.
spec.ts` drove the whole gesture green in Chromium **and** WebKit — Playwright dispatches the drag
straight into the engine, and there is no native window layer in front of it. The shipped app has
one, and that layer was eating the drag.

`scripts/wkwebview-probe` closed the "can this JS run in the engine we ship" hole and cannot answer
this question: a drag is not an expression. It arrives through AppKit's `NSDraggingDestination`
methods on the NSView, which is **above** the web content — in the view subclass wry installs.

## The mechanism it reproduces

wry 0.55 `src/wkwebview/drag_drop.rs` overrides `draggingEntered:`, `draggingUpdated:` and
`performDragOperation:`. Each asks Tauri's drag-drop handler first and calls `super` **only if that
handler declines**. `tauri-runtime-wry` installs a handler that returns `true` unconditionally
whenever the window's `dragDropEnabled` is true — and `true` is the default (`tauri-utils`
`default_true`). On a default config the overrides answer the drag themselves, WKWebView's own
implementation never runs, and the page is never told a drag happened.

`dragstart` still fires, because that is the drag **source** half and nothing intercepts it. So a
row picks up, follows the cursor, and can never land: no target ever arms, no drop ever fires. That
is what "drag and drop does not work" looked like from outside.

The fix is one line in `src-tauri/tauri.conf.json` — `"dragDropEnabled": false` — guarded by
`e2e/shell/workspace-dnd.spec.ts`.

## What it proves and does not prove

**Proves:** whether the native layer delivers or swallows a drag, in the real engine, for both
configurations, with no mouse and no accessibility grant — it calls the dragging destination methods
directly with a stub `NSDraggingInfo`, which is how AppKit calls them.

**Also measured** while diagnosing this, and recorded in the source header: with the native handler
off and a file dropped on a page that registers **no** drop handler at all, `draggingUpdated` answers
`NSDragOperationNone` and `location.href` is unchanged. Turning the native handler off does not
expose the webview to drop-to-navigate, so no global "swallow stray drops" guard is needed.

**Does not prove:** that the app's own drop targets file the right thing — that is the Playwright
spec's job, and it is a real oracle for the DOM half. It also says nothing about a signed build.

## Notes

- Needs a WindowServer session (the web view is attached to an off-screen window, because WebKit
  routes drags through the view hierarchy). It is therefore **not** wired into `scripts/ci.sh`; run
  it locally when a drag question comes up.
- `NSDragOperation` raw values in the output: `1` copy, `16` move, `0` none.
