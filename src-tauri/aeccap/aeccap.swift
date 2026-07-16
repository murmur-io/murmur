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
// Wall-clock self-cap. An explicit `maxSeconds` argument wins; absent / unparsable / ≤ 0 falls
// back to a DEFAULT 4h cap (mirrors `MAX_RECORDING_SECONDS`, audio/recorder.rs) — NEVER uncapped:
// an uncapped orphan means unbounded disk writes (the 91 GB stranded-WAV incident, and a system-
// audio sibling once outlived its parent by 7h20m).
let requestedMaxSeconds: Double = args.count >= 3 ? (Double(args[2]) ?? 0) : 0
let maxSeconds: Double = requestedMaxSeconds > 0 ? requestedMaxSeconds : 4 * 60 * 60

guard #available(macOS 10.15, *) else {
    FileHandle.standardError.write(Data("aeccap: requires macOS 10.15+\n".utf8))
    exit(4)
}

final class AecCapturer {
    private let outURL: URL
    private let engine = AVAudioEngine()
    private let lock = NSLock()
    private var file: AVAudioFile?
    private var monoFormat: AVAudioFormat?
    private var converter: AVAudioConverter?

    init(outURL: URL) { self.outURL = outURL }

    func start() throws {
        let input = engine.inputNode
        // Enable voice processing (AEC) on the input node BEFORE installing the tap. Throws
        // -10875/-10876 on a format/route mismatch — surfaced as exit 3 so the caller falls back
        // to the raw cpal mic.
        try input.setVoiceProcessingEnabled(true)

        // CONTAINMENT (see docs/research/2026-07-02-audio-echo-full-remediation.md):
        // 1) By default VPIO DUCKS all other apps' audio system-wide — it can quiet the very
        //    call being recorded and was observed killing a system-audio capture to ~-51 dB.
        //    macOS 14+ exposes the knob; pin it to minimum.
        // 2) Uplink AGC pumps the ASR feed's levels; disable for a level-faithful feed.
        if #available(macOS 14.0, *) {
            let duck = AVAudioVoiceProcessingOtherAudioDuckingConfiguration(
                enableAdvancedDucking: false, duckingLevel: .min)
            input.voiceProcessingOtherAudioDuckingConfiguration = duck
            input.isVoiceProcessingAGCEnabled = false
        }

        let tapFormat = input.outputFormat(forBus: 0)
        // ALWAYS persist a single MONO channel. VPIO can hand us a MULTI-CHANNEL device format (a
        // 9-channel aggregate input was seen in the field) — writing that verbatim ballooned the WAV
        // ~9x (a stuck session once stranded a 91 GB scratch file) AND the downstream Rust ASR feed
        // expects a compact mono track aligned with the raw mic. Downmix every buffer to mono so the
        // output is small, well-formed, and duration-faithful regardless of the device channel count.
        let mono = AVAudioFormat(
            commonFormat: .pcmFormatFloat32,
            sampleRate: tapFormat.sampleRate,
            channels: 1,
            interleaved: false)
        monoFormat = mono
        if tapFormat.channelCount > 1, let mono = mono {
            converter = AVAudioConverter(from: tapFormat, to: mono)
        }

        input.installTap(onBus: 0, bufferSize: 4096, format: tapFormat) { [weak self] buffer, _ in
            self?.write(buffer)
        }
        engine.prepare()
        try engine.start()
        FileHandle.standardError.write(
            Data(
                "aeccap: capturing (\(Int(tapFormat.sampleRate)) Hz, \(tapFormat.channelCount) ch in → 1 ch out)\n"
                    .utf8))
    }

    private func write(_ buffer: AVAudioPCMBuffer) {
        lock.lock()
        defer { lock.unlock() }
        // Downmix to mono when the input has >1 channel (same sample rate → no SRC); else write as-is.
        let outBuffer: AVAudioPCMBuffer
        if let converter = converter, let mono = monoFormat,
            let out = AVAudioPCMBuffer(pcmFormat: mono, frameCapacity: buffer.frameLength)
        {
            do {
                try converter.convert(to: out, from: buffer)
            } catch {
                return  // skip this buffer rather than write a malformed frame
            }
            outBuffer = out
        } else {
            outBuffer = buffer
        }
        if file == nil {
            file = try? AVAudioFile(
                forWriting: outURL, settings: outBuffer.format.settings,
                commonFormat: .pcmFormatFloat32, interleaved: false)
        }
        try? file?.write(from: outBuffer)
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

// Parent-liveness watchdog: this helper must never outlive the Murmur process that spawned it.
// The parent normally SIGTERMs us on Stop/quit — but a SIGKILL'd / crashed / hot-rebuilt parent
// sends nothing, and the orphan then keeps capturing until the self-cap. kqueue-backed
// EVFILT_PROC/NOTE_EXIT via DispatchSource — event-driven, no polling — routed into the SAME
// clean-stop path the signals use, so the WAV is flushed + closed before exit.
let parentPid = getppid()
// Already reparented to launchd (ppid 1) = the parent died while we were still launching. The
// Rust core always spawns this helper as a DIRECT child, so ppid 1 can only mean "orphaned before
// we could even observe the real parent" — watching launchd would wait forever.
if parentPid == 1 { requestStop(0) }
let parentWatch = DispatchSource.makeProcessSource(
    identifier: parentPid, eventMask: .exit, queue: sigQueue)
parentWatch.setEventHandler { requestStop(0) }
parentWatch.resume()
_ = Unmanaged.passRetained(parentWatch)  // keep alive for the process lifetime
// Close the registration race: if the parent died BEFORE the source was armed, its NOTE_EXIT
// already fired unseen and we were reparented (getppid() changed) — stop now instead of waiting
// on an event that will never come.
if getppid() != parentPid { requestStop(0) }

do {
    try capturer.start()
} catch {
    FileHandle.standardError.write(Data("aeccap: failed to start (\(error))\n".utf8))
    exit(3)
}

// Self-cap — ALWAYS armed (default 4h; see the `maxSeconds` derivation at the top).
// `wallDeadline` (not `deadline`): `DispatchTime` PAUSES while the machine sleeps, silently
// stretching the cap past its wall-clock intent — `DispatchWallTime` does not.
sigQueue.asyncAfter(wallDeadline: .now() + maxSeconds) { requestStop(0) }

RunLoop.main.run()
