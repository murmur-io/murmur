// wkwebview-probe — run JavaScript inside a REAL WKWebView and print the result.
//
// WHY THIS EXISTS
// ---------------
// Murmur's UI ships inside Tauri's WKWebView. The e2e suite runs in Playwright's
// Chromium and WebKit, and those are NEWER than the WKWebView on a given macOS.
// A modern web API can therefore work in every gate and still throw for the user.
//
// That is not hypothetical. 2026-08-04, PR #566/#568: `el.matches(":modal")` sat
// outside a `try`. `:modal` is a recent pseudo-class, so on the shipping engine
// `matches()` threw SyntaxError, the dialog never received `open`, and an
// unopened <dialog> is display:none. Six rounds of fixes went to the wrong layer
// because nothing could execute JS in the engine that was actually failing.
//
// This closes that hole. It needs no accessibility grant (unlike driving the app
// with AppleScript, which hangs the shell on a permission dialog), no signing,
// and no GUI: it is a plain command-line binary that loads a URL, waits for the
// page, evaluates an expression, and prints the JSON result.
//
// USAGE
//   wkwebview-probe --url http://localhost:1420/dashboards --eval 'document.title'
//   wkwebview-probe --url … --eval-file probe.js --timeout 30 --settle 2
//
// EXIT CODES
//   0  the expression evaluated; its JSON value is on stdout
//   1  bad arguments
//   2  navigation failed
//   3  the expression threw (the JS error text is on stderr)
//   4  timed out
//
// The probe deliberately prints ONLY the evaluated value on stdout, so callers
// can pipe it into jq. Diagnostics go to stderr.

import Foundation
import WebKit

// ── arguments ────────────────────────────────────────────────────────────────

func argValue(_ name: String) -> String? {
    let args = CommandLine.arguments
    guard let i = args.firstIndex(of: name), i + 1 < args.count else { return nil }
    return args[i + 1]
}

func fail(_ message: String, code: Int32) -> Never {
    FileHandle.standardError.write(Data(("wkwebview-probe: " + message + "\n").utf8))
    exit(code)
}

guard let urlString = argValue("--url"), let url = URL(string: urlString) else {
    fail("--url <URL> is required", code: 1)
}

var script: String
if let inline = argValue("--eval") {
    script = inline
} else if let path = argValue("--eval-file") {
    guard let body = try? String(contentsOfFile: path, encoding: .utf8) else {
        fail("cannot read --eval-file \(path)", code: 1)
    }
    script = body
} else {
    fail("one of --eval <expr> or --eval-file <path> is required", code: 1)
}

let timeout = Double(argValue("--timeout") ?? "30") ?? 30
// Seconds to wait AFTER load before evaluating. An Angular app needs a beat to
// bootstrap and render; without it the probe reads an empty <app-root>.
let settle = Double(argValue("--settle") ?? "2") ?? 2

// ── the probe ────────────────────────────────────────────────────────────────

final class Probe: NSObject, WKNavigationDelegate {
    let webView: WKWebView
    private let script: String
    private let settle: Double

    init(script: String, settle: Double) {
        let config = WKWebViewConfiguration()
        // Match the app: Tauri serves the dev app over http on localhost.
        config.preferences.setValue(true, forKey: "allowFileAccessFromFileURLs")
        self.webView = WKWebView(frame: .init(x: 0, y: 0, width: 1440, height: 900),
                                 configuration: config)
        self.script = script
        self.settle = settle
        super.init()
        webView.navigationDelegate = self
    }

    func load(_ url: URL) {
        webView.load(URLRequest(url: url))
    }

    func webView(_ webView: WKWebView, didFinish navigation: WKNavigation!) {
        // Give the SPA time to bootstrap, then evaluate.
        DispatchQueue.main.asyncAfter(deadline: .now() + settle) { [weak self] in
            self?.evaluate()
        }
    }

    func webView(_ webView: WKWebView, didFail navigation: WKNavigation!, withError error: Error) {
        fail("navigation failed: \(error.localizedDescription)", code: 2)
    }

    func webView(_ webView: WKWebView,
                 didFailProvisionalNavigation navigation: WKNavigation!,
                 withError error: Error) {
        fail("navigation failed: \(error.localizedDescription)", code: 2)
    }

    private func evaluate() {
        // Wrap in an async IIFE so callers may use `await`, and JSON-encode the
        // result so structured values survive the bridge (WKWebView only returns
        // a small set of primitive types directly).
        // NOTE the leading `return`: callAsyncJavaScript treats the string as the
        // BODY of an async function, so an expression alone evaluates to undefined.
        let wrapped = """
        try {
          const value = await (async () => { \(script) })();
          return JSON.stringify({ ok: true, value: value === undefined ? null : value });
        } catch (e) {
          return JSON.stringify({ ok: false, error: String((e && e.stack) || e) });
        }
        """
        webView.callAsyncJavaScript(wrapped, in: nil, in: .page) { result in
            switch result {
            case .success(let value):
                guard let json = value as? String,
                      let data = json.data(using: .utf8),
                      let obj = try? JSONSerialization.jsonObject(with: data) as? [String: Any]
                else {
                    fail("could not decode the probe result", code: 3)
                }
                if let ok = obj["ok"] as? Bool, ok {
                    let payload = obj["value"] ?? NSNull()
                    let out = (try? JSONSerialization.data(withJSONObject: ["value": payload]))
                        .flatMap { String(data: $0, encoding: .utf8) } ?? "{\"value\":null}"
                    print(out)
                    exit(0)
                } else {
                    fail("evaluation threw: \(obj["error"] as? String ?? "unknown")", code: 3)
                }
            case .failure(let error):
                fail("evaluation failed: \(error.localizedDescription)", code: 3)
            }
        }
    }
}

let probe = Probe(script: script, settle: settle)
probe.load(url)

DispatchQueue.main.asyncAfter(deadline: .now() + timeout) {
    fail("timed out after \(Int(timeout))s", code: 4)
}

// WKWebView needs a run loop; there is no window, so nothing is shown.
RunLoop.main.run()
