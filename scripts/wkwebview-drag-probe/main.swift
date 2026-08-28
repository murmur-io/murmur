// wkwebview-drag-probe — does a drag reach the PAGE inside a real WKWebView?
//
// WHY THIS EXISTS
// ---------------
// Murmur's sidebar drag-and-drop worked in every gate and in neither shipped
// build. The Playwright suite (e2e/shell/workspace-dnd.spec.ts) drove the whole
// gesture green in Chromium AND WebKit, because Playwright dispatches the drag
// straight into the engine — there is no native window layer in front of it.
// The shipped app has one, and that layer was eating the drag.
//
// `scripts/wkwebview-probe` closed the "can this JS run in the engine we ship"
// hole. It cannot answer this question: a drag is not an expression, it arrives
// through AppKit's NSDraggingDestination methods on the NSView, and what breaks
// it lives ABOVE the web content, in the view subclass wry installs.
//
// WHAT IT REPRODUCES
// ------------------
// wry 0.55 `src/wkwebview/drag_drop.rs` overrides `draggingEntered:`,
// `draggingUpdated:` and `performDragOperation:` on its WKWebView subclass. Each
// asks Tauri's drag-drop handler first and calls `super` ONLY if that handler
// declines. `tauri-runtime-wry` installs a handler that returns `true`
// unconditionally whenever the window's `dragDropEnabled` is true — and true is
// the DEFAULT. So on a default config the overrides answer the drag themselves,
// WKWebView's own implementation never runs, and the page is never told a drag
// happened: no dragenter, no dragover, no drop. `dragstart` still fires (that is
// the SOURCE half, which nothing intercepts), so a row picks up and follows the
// cursor — the gesture looks alive and can never land.
//
// This probe builds both configurations and reports what the page saw.
//
// USAGE
//   swiftc -O -o /tmp/dragprobe scripts/wkwebview-drag-probe/main.swift
//   /tmp/dragprobe               # print the comparison table
//   /tmp/dragprobe --self-test   # exit 1 unless the asymmetry still holds
//   /tmp/dragprobe --file        # drag a FILE rather than text
//
// Needs no accessibility grant, no signing and no mouse: it calls the dragging
// destination methods directly with a stub NSDraggingInfo, which is exactly how
// AppKit calls them.
//
// MEASURED 2026-08-28 (macOS 25.5, arm64):
//   swallow (dragDropEnabled true)  → page saw {enter:0, over:0, drop:0}
//   forward (dragDropEnabled false) → page saw {enter:1, over:1, drop:1}, op=move
//   unhandled file drop, forwarding → draggingUpdated answers NONE, href unchanged
//     (WebKit refuses a drop the page did not accept, so turning the native
//      handler off does NOT expose the webview to drop-to-navigate)
//
// EXIT CODES
//   0  ran (and, under --self-test, the asymmetry held)
//   1  --self-test found the asymmetry broken
//   2  a page failed to load

import AppKit
import WebKit

let wantFile = CommandLine.arguments.contains("--file")
let selfTest = CommandLine.arguments.contains("--self-test")

/// The stub AppKit itself would pass in. Only the members WebKit reads matter.
final class StubDraggingInfo: NSObject, NSDraggingInfo {
    var draggingDestinationWindow: NSWindow?
    var draggingSourceOperationMask: NSDragOperation = [.copy, .move]
    var draggingLocation: NSPoint = .zero
    var draggedImageLocation: NSPoint = .zero
    var draggedImage: NSImage?
    var draggingPasteboard: NSPasteboard
    var draggingSource: Any?
    var draggingSequenceNumber: Int = 1
    var draggingFormation: NSDraggingFormation = .default
    var animatesToDestination: Bool = false
    var numberOfValidItemsForDrop: Int = 1
    var springLoadingHighlight: NSSpringLoadingHighlight = .none

    init(pasteboard: NSPasteboard, location: NSPoint, window: NSWindow?) {
        self.draggingPasteboard = pasteboard
        self.draggingLocation = location
        self.draggingDestinationWindow = window
        super.init()
    }

    func slideDraggedImage(to screenPoint: NSPoint) {}
    override func namesOfPromisedFilesDropped(atDestination dropDestination: URL) -> [String]? { nil }
    func enumerateDraggingItems(
        options enumOpts: NSDraggingItemEnumerationOptions,
        for view: NSView?,
        classes classArray: [AnyClass],
        searchOptions: [NSPasteboard.ReadingOptionKey: Any],
        using block: (NSDraggingItem, Int, UnsafeMutablePointer<ObjCBool>) -> Void
    ) {}
    func resetSpringLoading() {}
}

/// Mirrors wry's override, with its one decision exposed as a flag.
final class ProbeWebView: WKWebView {
    /// true = Tauri's handler claimed the drag (dragDropEnabled true, the default).
    var swallow = true

    override func draggingEntered(_ sender: NSDraggingInfo) -> NSDragOperation {
        swallow ? .copy : super.draggingEntered(sender)
    }
    override func draggingUpdated(_ sender: NSDraggingInfo) -> NSDragOperation {
        swallow ? .copy : super.draggingUpdated(sender)
    }
    override func performDragOperation(_ sender: NSDraggingInfo) -> Bool {
        swallow ? true : super.performDragOperation(sender)
    }
    override func draggingExited(_ sender: NSDraggingInfo?) {
        if !swallow { super.draggingExited(sender) }
    }
}

let page = """
<!doctype html><html><body style="margin:0">
<div id="t" style="width:100vw;height:100vh">drop target</div>
<script>
window.__seen = { enter: 0, over: 0, drop: 0 };
const t = document.getElementById("t");
// preventDefault is what makes a target accept a drop — the same thing
// FolderDropDirective does.
t.addEventListener("dragenter", e => { e.preventDefault(); window.__seen.enter++; });
t.addEventListener("dragover",  e => { e.preventDefault(); window.__seen.over++; });
t.addEventListener("drop",      e => { e.preventDefault(); window.__seen.drop++; });
</script></body></html>
"""

final class LoadWatcher: NSObject, WKNavigationDelegate {
    var finished = false
    func webView(_ webView: WKWebView, didFinish navigation: WKNavigation!) { finished = true }
}

func pump(_ seconds: Double) {
    let deadline = Date().addingTimeInterval(seconds)
    while Date() < deadline {
        RunLoop.current.run(mode: .default, before: Date().addingTimeInterval(0.02))
    }
}

struct Result {
    let enteredOp: UInt
    let updatedOp: UInt
    let seen: String
}

func run(swallow: Bool) -> Result {
    let window = NSWindow(
        contentRect: NSRect(x: 0, y: 0, width: 600, height: 400),
        styleMask: [.titled], backing: .buffered, defer: false)
    let web = ProbeWebView(frame: NSRect(x: 0, y: 0, width: 600, height: 400),
                           configuration: WKWebViewConfiguration())
    web.swallow = swallow
    let watcher = LoadWatcher()
    web.navigationDelegate = watcher
    window.contentView = web
    window.orderFrontRegardless()
    web.loadHTMLString(page, baseURL: URL(string: "http://localhost/"))

    var waited = 0.0
    while !watcher.finished && waited < 10 { pump(0.05); waited += 0.05 }
    guard watcher.finished else {
        FileHandle.standardError.write(Data("wkwebview-drag-probe: page load timed out\n".utf8))
        exit(2)
    }
    pump(0.5)

    let pasteboard = NSPasteboard(name: NSPasteboard.Name("murmur.wkwebview-drag-probe"))
    pasteboard.clearContents()
    if wantFile {
        pasteboard.writeObjects([URL(fileURLWithPath: "/etc/hosts") as NSURL])
    } else {
        pasteboard.setString("murmur", forType: .string)
    }

    let info = StubDraggingInfo(pasteboard: pasteboard,
                                location: NSPoint(x: 300, y: 200),
                                window: window)
    let entered = web.draggingEntered(info)
    pump(0.4)
    let updated = web.draggingUpdated(info)
    pump(0.4)
    _ = web.performDragOperation(info)
    pump(0.8)

    var seen = "?"
    let done = DispatchSemaphore(value: 0)
    web.evaluateJavaScript("JSON.stringify(window.__seen)") { value, error in
        seen = (value as? String) ?? "error: \(String(describing: error))"
        done.signal()
    }
    while done.wait(timeout: .now()) == .timedOut { pump(0.05) }

    window.orderOut(nil)
    return Result(enteredOp: entered.rawValue, updatedOp: updated.rawValue, seen: seen)
}

NSApplication.shared.setActivationPolicy(.accessory)

let swallowed = run(swallow: true)
let forwarded = run(swallow: false)

print("dragging \(wantFile ? "a file" : "text") onto a page whose target accepts drops\n")
print("  dragDropEnabled: true  (wry answers, WKWebView never sees it)")
print("    draggingEntered=\(swallowed.enteredOp) draggingUpdated=\(swallowed.updatedOp) page saw \(swallowed.seen)")
print("  dragDropEnabled: false (wry declines, WKWebView handles it)")
print("    draggingEntered=\(forwarded.enteredOp) draggingUpdated=\(forwarded.updatedOp) page saw \(forwarded.seen)")

if selfTest {
    let blocked = swallowed.seen.contains("\"enter\":0")
        && swallowed.seen.contains("\"over\":0")
        && swallowed.seen.contains("\"drop\":0")
    let delivered = forwarded.seen.contains("\"enter\":1")
        && forwarded.seen.contains("\"over\":1")
        && forwarded.seen.contains("\"drop\":1")
    if !blocked {
        FileHandle.standardError.write(Data(
            "wkwebview-drag-probe: expected the swallowing config to starve the page, saw \(swallowed.seen)\n".utf8))
        exit(1)
    }
    if !delivered {
        FileHandle.standardError.write(Data(
            "wkwebview-drag-probe: expected the forwarding config to deliver the drag, saw \(forwarded.seen)\n".utf8))
        exit(1)
    }
    print("\nself-test OK — the native layer, not the page, decides whether a drag exists")
}
