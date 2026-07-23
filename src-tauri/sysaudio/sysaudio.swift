// Murmur — system-audio capture sidecar (macOS 13+, ScreenCaptureKit).
//
// Captures the system audio output to a WAV file. Spawned by the Rust core
// (`audio::system::SystemAudioRecorder`); the Rust side stops it with SIGINT/SIGTERM
// and then reads the WAV. Phase 0 records the mic only; this adds the "other side
// of the call".
//
// Usage:  sysaudio <output.wav> [maxSeconds]
// Exit:   0 finalized · 2 bad args · 3 pre-ready failure · 4 unsupported · 6 parent-loss hard bound
//
// ⚠️ RUNTIME-UNVERIFIED in a headless build: capturing real system audio needs an
// interactive desktop session + the Screen Recording (TCC) permission + live audio.
// Compilation is verified (swiftc); end-to-end capture must be confirmed on a real Mac.

import AVFoundation
import Darwin
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

// Every stop source shares this phase gate. Before `start()` completes, capture objects are only
// partially initialized and teardown is unsafe, so fail fast with NONZERO status. Once ready, the
// first request owns clean finalization; later signal/EOF/self-cap races are idempotent no-ops.
enum CapturePhase { case starting, ready, stopping }
var capturePhase = CapturePhase.starting
let stopLock = NSLock()
func requestStop(_ code: Int32) {
    stopLock.lock()
    switch capturePhase {
    case .stopping:
        stopLock.unlock()
        return
    case .starting:
        capturePhase = .stopping
        stopLock.unlock()
        // A callback may already have emitted a tiny partial file, but teardown is not safe until
        // `start()` returns. `_exit(3)` prevents Rust from adopting it as a finalized success.
        _exit(3)
    case .ready:
        capturePhase = .stopping
        stopLock.unlock()
        Task { await capturer.stop(); exit(code) }
    }
}
func markCaptureReady() {
    stopLock.lock()
    if case .starting = capturePhase { capturePhase = .ready }
    stopLock.unlock()
}
let sigQueue = DispatchQueue(label: "meetnotes.sig")
for sig in [SIGINT, SIGTERM] {
    signal(sig, SIG_IGN)
    let src = DispatchSource.makeSignalSource(signal: sig, queue: sigQueue)
    src.setEventHandler { requestStop(0) }
    src.resume()
    // keep the source alive for the process lifetime
    _ = Unmanaged.passRetained(src)
}

// Exact parent lifetime capability: Rust owns the only stdin writer. EOF therefore means that
// exact recorder owner died or dropped its capability; unlike PID/kqueue observation it has no
// registration race, reparenting ambiguity, or PID-reuse window. Retry interrupted reads; EOF and
// every other read failure fail closed through the same phase-aware stop gate. Queue stop work on
// `sigQueue`, and arm an independent hard exit first: if capture finalization wedges after its exact
// owner disappears, the helper still cannot outlive that owner indefinitely. Exit 6 is deliberately
// not a finalized-file proof, so Rust preserves recovery metadata but never adopts the partial WAV.
DispatchQueue(label: "meetnotes.parent-lifetime", qos: .utility).async {
    var byte: UInt8 = 0
    while true {
        let count = withUnsafeMutableBytes(of: &byte) { buffer in
            Darwin.read(STDIN_FILENO, buffer.baseAddress, buffer.count)
        }
        if count > 0 { continue }
        if count < 0 && errno == EINTR { continue }
        DispatchQueue.global(qos: .utility).asyncAfter(wallDeadline: .now() + 5) { _exit(6) }
        sigQueue.async { requestStop(0) }
        return
    }
}

// Self-cap — ALWAYS armed (default 4h; see the `maxSeconds` derivation at the top), and armed
// INDEPENDENT of capture start so even a wedged SCK setup can't leave the process alive
// unbounded. `wallDeadline` (not `deadline`): `DispatchTime` PAUSES while the machine sleeps,
// silently stretching the cap past its wall-clock intent — `DispatchWallTime` does not.
sigQueue.asyncAfter(wallDeadline: .now() + maxSeconds) { requestStop(0) }

Task {
    do {
        try await capturer.start()
        markCaptureReady()
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
