// Murmur — AEC microphone-capture helper via AVAudioEngine VoiceProcessingIO.
//
// Captures the mic with the system VOICE PROCESSING (acoustic echo cancellation + noise
// suppression) enabled, into a WAV. Used as the ASR mic feed when the user records WITHOUT
// headphones (so the other participants' audio on the speakers is cancelled out of the mic). The
// raw cpal mic stays the archive source — this helper runs in PARALLEL and is purely best-effort.
//
// Usage:  aeccap <output.wav> [maxSeconds]
// Exit:   0 ok · 2 bad args · 3 engine/VPIO failed (e.g. -10875/-10876) · 4 unsupported OS
//
// ⚠️ RUNTIME-UNVERIFIED headless: whether this VPIO graph coexists with cpal on the same mic, and
// whether AEC actually cancels the echo, need a SIGNED build on a real Mac with a live call.
// Compilation (swiftc against the SDK) IS verified.

import AVFoundation
import Foundation

let args = CommandLine.arguments
guard args.count >= 2 else {
    FileHandle.standardError.write(Data("usage: aeccap <output.wav> [maxSeconds]\n".utf8))
    exit(2)
}
let outURL = URL(fileURLWithPath: args[1])
let maxSeconds: Double = args.count >= 3 ? (Double(args[2]) ?? 0) : 0

guard #available(macOS 10.15, *) else {
    FileHandle.standardError.write(Data("aeccap: requires macOS 10.15+\n".utf8))
    exit(4)
}

final class AecCapturer {
    private let outURL: URL
    private let engine = AVAudioEngine()
    private let lock = NSLock()
    private var file: AVAudioFile?

    init(outURL: URL) { self.outURL = outURL }

    func start() throws {
        let input = engine.inputNode
        // Enable voice processing (AEC) on the input node BEFORE installing the tap. Throws
        // -10875/-10876 on a format/route mismatch — surfaced as exit 3 so the caller falls back
        // to the raw cpal mic.
        try input.setVoiceProcessingEnabled(true)

        let format = input.outputFormat(forBus: 0)
        input.installTap(onBus: 0, bufferSize: 4096, format: format) { [weak self] buffer, _ in
            self?.write(buffer, format: format)
        }
        engine.prepare()
        try engine.start()
        FileHandle.standardError.write(Data("aeccap: capturing\n".utf8))
    }

    private func write(_ buffer: AVAudioPCMBuffer, format: AVAudioFormat) {
        lock.lock()
        defer { lock.unlock() }
        if file == nil {
            file = try? AVAudioFile(
                forWriting: outURL, settings: format.settings,
                commonFormat: .pcmFormatFloat32, interleaved: false)
        }
        try? file?.write(from: buffer)
    }

    func stop() {
        engine.inputNode.removeTap(onBus: 0)
        engine.stop()
        lock.lock()
        file = nil  // releasing the AVAudioFile flushes + closes it
        lock.unlock()
    }
}

let capturer = AecCapturer(outURL: outURL)

var stopping = false
let stopLock = NSLock()
func requestStop(_ code: Int32) {
    stopLock.lock()
    if stopping {
        stopLock.unlock()
        return
    }
    stopping = true
    stopLock.unlock()
    capturer.stop()
    exit(code)
}

let sigQueue = DispatchQueue(label: "murmur.aeccap.sig")
for sig in [SIGINT, SIGTERM] {
    signal(sig, SIG_IGN)
    let src = DispatchSource.makeSignalSource(signal: sig, queue: sigQueue)
    src.setEventHandler { requestStop(0) }
    src.resume()
    _ = Unmanaged.passRetained(src)  // keep alive for the process lifetime
}

do {
    try capturer.start()
} catch {
    FileHandle.standardError.write(Data("aeccap: failed to start (\(error))\n".utf8))
    exit(3)
}

if maxSeconds > 0 {
    sigQueue.asyncAfter(deadline: .now() + maxSeconds) { requestStop(0) }
}

RunLoop.main.run()
