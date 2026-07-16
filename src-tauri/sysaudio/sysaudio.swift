// MeetNotes — system-audio capture sidecar (macOS 13+, ScreenCaptureKit).
//
// Captures the system audio output to a WAV file. Spawned by the Rust core
// (`audio::system::SystemAudioRecorder`); the Rust side stops it with SIGINT/SIGTERM
// and then reads the WAV. Phase 0 records the mic only; this adds the "other side
// of the call".
//
// Usage:  sysaudio <output.wav> [maxSeconds]
// Exit:   0 ok · 2 bad args · 3 no Screen-Recording permission / no content · 4 unsupported
//
// ⚠️ RUNTIME-UNVERIFIED in a headless build: capturing real system audio needs an
// interactive desktop session + the Screen Recording (TCC) permission + live audio.
// Compilation is verified (swiftc); end-to-end capture must be confirmed on a real Mac.

import AVFoundation
import Foundation
import ScreenCaptureKit

guard #available(macOS 13.0, *) else {
    FileHandle.standardError.write(Data("sysaudio: requires macOS 13+\n".utf8))
    exit(4)
}

let args = CommandLine.arguments
guard args.count >= 2 else {
    FileHandle.standardError.write(Data("usage: sysaudio <output.wav> [maxSeconds]\n".utf8))
    exit(2)
}
let outURL = URL(fileURLWithPath: args[1])
// Wall-clock self-cap. An explicit `maxSeconds` argument wins; absent / unparsable / ≤ 0 falls
// back to a DEFAULT 4h cap (mirrors `MAX_RECORDING_SECONDS`, audio/recorder.rs) — NEVER uncapped:
// an uncapped orphan means unbounded disk writes (a system-audio sibling once outlived its
// parent by 7h20m and wrote 2+ GB of dead-session audio).
let requestedMaxSeconds: Double = args.count >= 3 ? (Double(args[2]) ?? 0) : 0
let maxSeconds: Double = requestedMaxSeconds > 0 ? requestedMaxSeconds : 4 * 60 * 60

@available(macOS 13.0, *)
final class Capturer: NSObject, SCStreamOutput, SCStreamDelegate {
    private let outURL: URL
    private let lock = NSLock()
    private var file: AVAudioFile?
    private var stream: SCStream?

    init(outURL: URL) { self.outURL = outURL }

    func start() async throws {
        // Triggers the Screen-Recording permission prompt; throws if denied.
        let content = try await SCShareableContent.current
        guard let display = content.displays.first else {
            FileHandle.standardError.write(Data("sysaudio: no display available\n".utf8))
            exit(3)
        }
        // Global-minus-self: capture the whole display's audio EXCEPT Murmur's own output, so
        // we never re-capture our own playback / notification sounds. com.meetnotes.app is the
        // immutable bundle id of the main app.
        let ownBundleID = "com.meetnotes.app"
        let excluded = content.applications.filter { $0.bundleIdentifier == ownBundleID }
        let filter = SCContentFilter(
            display: display, excludingApplications: excluded, exceptingWindows: [])

        let config = SCStreamConfiguration()
        config.capturesAudio = true
        config.sampleRate = 48_000
        config.channelCount = 1
        // We do not use the video stream; keep it minimal.
        config.width = 2
        config.height = 2
        config.minimumFrameInterval = CMTime(value: 1, timescale: 1)

        let stream = SCStream(filter: filter, configuration: config, delegate: self)
        try stream.addStreamOutput(
            self, type: .audio,
            sampleHandlerQueue: DispatchQueue(label: "meetnotes.audio"))
        self.stream = stream
        try await stream.startCapture()
    }

    /// Sync (non-async) so the NSLock is never held across an `await`.
    private func closeFile() {
        lock.lock()
        file = nil  // releasing the AVAudioFile flushes + closes it
        lock.unlock()
    }

    func stop() async {
        if let s = stream { try? await s.stopCapture() }
        closeFile()
    }

    // SCStreamOutput
    func stream(
        _ stream: SCStream, didOutputSampleBuffer sampleBuffer: CMSampleBuffer,
        of type: SCStreamOutputType
    ) {
        guard type == .audio, sampleBuffer.isValid else { return }
        try? sampleBuffer.withAudioBufferList { abl, _ in
            guard
                let asbd = sampleBuffer.formatDescription?.audioStreamBasicDescription,
                let format = AVAudioFormat(
                    standardFormatWithSampleRate: asbd.mSampleRate,
                    channels: asbd.mChannelsPerFrame),
                let pcm = AVAudioPCMBuffer(pcmFormat: format, bufferListNoCopy: abl.unsafePointer)
            else { return }

            lock.lock()
            defer { lock.unlock() }
            if file == nil {
                // Anchor line for the Rust wall-clock merge: the true capture start (vs the
                // process-spawn instant, which precedes SCK setup by hundreds of ms).
                FileHandle.standardError.write(Data("sysaudio: first-frame\n".utf8))
                file = try? AVAudioFile(
                    forWriting: outURL, settings: format.settings,
                    commonFormat: .pcmFormatFloat32, interleaved: false)
            }
            try? file?.write(from: pcm)
        }
    }

    // SCStreamDelegate
    func stream(_ stream: SCStream, didStopWithError error: Error) {
        FileHandle.standardError.write(Data("sysaudio: stream stopped: \(error)\n".utf8))
    }
}

let capturer = Capturer(outURL: outURL)

// Clean shutdown on SIGINT/SIGTERM from the parent (Rust) process.
var stopping = false
func requestStop() {
    if stopping { return }
    stopping = true
    Task { await capturer.stop(); exit(0) }
}
let sigQueue = DispatchQueue(label: "meetnotes.sig")
for sig in [SIGINT, SIGTERM] {
    signal(sig, SIG_IGN)
    let src = DispatchSource.makeSignalSource(signal: sig, queue: sigQueue)
    src.setEventHandler(handler: requestStop)
    src.resume()
    // keep the source alive for the process lifetime
    _ = Unmanaged.passRetained(src)
}

// Parent-liveness watchdog: this helper must never outlive the Murmur process that spawned it.
// The parent normally SIGTERMs us on Stop/quit — but a SIGKILL'd / crashed / hot-rebuilt parent
// sends nothing, and the orphan then keeps capturing until the self-cap (a sibling helper once
// outlived its parent by 7h20m). kqueue-backed EVFILT_PROC/NOTE_EXIT via DispatchSource —
// event-driven, no polling — routed into the SAME clean-stop path the signals use, so the WAV is
// flushed + closed before exit.
let parentPid = getppid()
// Already reparented to launchd (ppid 1) = the parent died while we were still launching. The
// Rust core always spawns this helper as a DIRECT child, so ppid 1 can only mean "orphaned before
// we could even observe the real parent" — watching launchd would wait forever.
if parentPid == 1 { requestStop() }
let parentWatch = DispatchSource.makeProcessSource(
    identifier: parentPid, eventMask: .exit, queue: sigQueue)
parentWatch.setEventHandler(handler: requestStop)
parentWatch.resume()
_ = Unmanaged.passRetained(parentWatch)  // keep alive for the process lifetime
// Close the registration race: if the parent died BEFORE the source was armed, its NOTE_EXIT
// already fired unseen and we were reparented (getppid() changed) — stop now instead of waiting
// on an event that will never come.
if getppid() != parentPid { requestStop() }

// Self-cap — ALWAYS armed (default 4h; see the `maxSeconds` derivation at the top), and armed
// INDEPENDENT of capture start so even a wedged SCK setup can't leave the process alive
// unbounded. `wallDeadline` (not `deadline`): `DispatchTime` PAUSES while the machine sleeps,
// silently stretching the cap past its wall-clock intent — `DispatchWallTime` does not.
sigQueue.asyncAfter(wallDeadline: .now() + maxSeconds) { requestStop() }

Task {
    do {
        try await capturer.start()
        FileHandle.standardError.write(Data("sysaudio: capturing\n".utf8))
    } catch {
        FileHandle.standardError.write(
            Data(
                "sysaudio: failed to start (\(error)) — likely missing Screen Recording permission\n"
                    .utf8))
        exit(3)
    }
}

RunLoop.main.run()
