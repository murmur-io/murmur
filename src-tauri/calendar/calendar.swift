// Murmur — local Calendar context helper via EventKit.
//
// Surfaces meeting context (title, attendees, agenda/notes) from the user's LOCAL macOS Calendar
// so the brain / pre-meeting brief can use "who's in this meeting + the agenda". This is the
// CALENDAR source of the multi-source roadmap: local, ZERO-OAuth, on-device. Slack/Jira stay
// deferred.
//
// Usage:   meetnotes-calendar [backMinutes] [forwardMinutes]
//          (defaults: 60 back, 720 forward — i.e. now-1h .. now+12h)
// Output:  ONE JSON object on stdout, ALWAYS exit 0:
//          {"status":"ok|denied|empty|error","events":[
//             {"id","title","start"(ISO8601),"end"(ISO8601),"attendees":[..],"notes"}, ... ]}
// Exit:    ALWAYS 0. A missing permission / no events / any thrown error → a well-formed JSON
//          envelope with the right status and (usually) an empty events array. NEVER crash, never
//          hang: a hard wall-clock watchdog guarantees the process exits even if EventKit stalls.
//
// CRASH-SAFE CONTRACT (the Rust side relies on this): the helper is a SEPARATE process, so the
// only way it can hurt the app is by hanging or by producing garbage. It does neither — the
// watchdog bounds the lifetime and every exit path prints a parseable envelope.
//
// ⚠️ RUNTIME-UNVERIFIED headless: that EventKit returns REAL events needs the Calendars (TCC)
// permission granted on a SIGNED build on a real Mac. Compilation (swiftc against the SDK) and
// the graceful denied/empty/error envelopes ARE the verified surface.

import EventKit
import Foundation

// MARK: - Output envelope (hand-rolled JSON so a single malformed field can't abort the whole emit)

func jsonEscape(_ s: String) -> String {
    var out = ""
    out.reserveCapacity(s.count + 2)
    for ch in s.unicodeScalars {
        switch ch {
        case "\"": out += "\\\""
        case "\\": out += "\\\\"
        case "\n": out += "\\n"
        case "\r": out += "\\r"
        case "\t": out += "\\t"
        default:
            if ch.value < 0x20 {
                out += String(format: "\\u%04x", ch.value)
            } else {
                out.unicodeScalars.append(ch)
            }
        }
    }
    return out
}

func emit(status: String, eventsJson: [String]) -> Never {
    let body = eventsJson.joined(separator: ",")
    let line = "{\"status\":\"\(status)\",\"events\":[\(body)]}"
    FileHandle.standardOutput.write(Data((line + "\n").utf8))
    exit(0)
}

func emitEmpty(_ status: String) -> Never { emit(status: status, eventsJson: []) }

// MARK: - Watchdog — guarantee the process can NEVER hang (EventKit auth/fetch could stall).

let watchdog = DispatchQueue(label: "murmur.calendar.watchdog")
watchdog.asyncAfter(deadline: .now() + 8.0) {
    // Timed out waiting on permission / fetch. Emit an honest envelope and leave.
    emitEmpty("error")
}

// MARK: - Window args (bounded, sane defaults; never trust the parse to be valid)

let args = CommandLine.arguments
let backMinutes = args.count >= 2 ? max(0.0, min(Double(args[1]) ?? 60.0, 7 * 24 * 60)) : 60.0
let forwardMinutes = args.count >= 3 ? max(0.0, min(Double(args[2]) ?? 720.0, 7 * 24 * 60)) : 720.0

let store = EKEventStore()

let iso = ISO8601DateFormatter()
iso.formatOptions = [.withInternetDateTime]

func attendeeName(_ p: EKParticipant) -> String? {
    if let name = p.name, !name.isEmpty { return name }
    // Fall back to the mailto: email in the URL if there's no display name.
    let urlStr = p.url.absoluteString
    if urlStr.lowercased().hasPrefix("mailto:") {
        let email = String(urlStr.dropFirst("mailto:".count))
        return email.isEmpty ? nil : email
    }
    return nil
}

func eventJson(_ e: EKEvent) -> String {
    var fields: [String] = []
    let id = e.eventIdentifier ?? ""
    fields.append("\"id\":\"\(jsonEscape(id))\"")
    fields.append("\"title\":\"\(jsonEscape(e.title ?? ""))\"")
    if let s = e.startDate { fields.append("\"start\":\"\(jsonEscape(iso.string(from: s)))\"") } else {
        fields.append("\"start\":null")
    }
    if let en = e.endDate { fields.append("\"end\":\"\(jsonEscape(iso.string(from: en)))\"") } else {
        fields.append("\"end\":null")
    }
    let names = (e.attendees ?? []).compactMap(attendeeName)
    let attJson = names.map { "\"\(jsonEscape($0))\"" }.joined(separator: ",")
    fields.append("\"attendees\":[\(attJson)]")
    let notes = e.notes ?? ""
    fields.append("\"notes\":\"\(jsonEscape(notes))\"")
    return "{\(fields.joined(separator: ","))}"
}

func fetchAndEmit() {
    let now = Date()
    let start = now.addingTimeInterval(-backMinutes * 60)
    let end = now.addingTimeInterval(forwardMinutes * 60)
    let predicate = store.predicateForEvents(withStart: start, end: end, calendars: nil)
    let events = store.events(matching: predicate).sorted {
        ($0.startDate ?? .distantPast) < ($1.startDate ?? .distantPast)
    }
    if events.isEmpty { emitEmpty("empty") }
    let json = events.map(eventJson)
    emit(status: "ok", eventsJson: json)
}

// MARK: - Permission → fetch. Handle BOTH the macOS 14+ and legacy auth APIs, degrade gracefully.

func requestThenFetch() {
    let completion: (Bool, Error?) -> Void = { granted, _ in
        if granted {
            fetchAndEmit()
        } else {
            emitEmpty("denied")
        }
    }
    if #available(macOS 14.0, *) {
        store.requestFullAccessToEvents(completion: completion)
    } else {
        store.requestAccess(to: .event, completion: completion)
    }
}

requestThenFetch()

// Keep the process alive for the async EventKit callback; the watchdog bounds it.
RunLoop.main.run()
