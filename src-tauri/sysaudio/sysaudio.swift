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
let maxSeconds: Double = args.count >= 3 ? (Double(args[2]) ?? 0) : 0

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
        let filter = SCContentFilter(
            display: display, excludingApplications: [], exceptingWindows: [])

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

Task {
    do {
        try await capturer.start()
        FileHandle.standardError.write(Data("sysaudio: capturing\n".utf8))
        if maxSeconds > 0 {
            try await Task.sleep(nanoseconds: UInt64(maxSeconds * 1_000_000_000))
            requestStop()
        }
    } catch {
        FileHandle.standardError.write(
            Data(
                "sysaudio: failed to start (\(error)) — likely missing Screen Recording permission\n"
                    .utf8))
        exit(3)
    }
}

RunLoop.main.run()
