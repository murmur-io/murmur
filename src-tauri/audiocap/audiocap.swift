// Murmur — system-audio capture sidecar via the Core Audio PROCESS TAP API (macOS 14.4+).
//
// Captures the whole system audio output EXCEPT our own app (global-minus-self) into a WAV,
// using a process tap + a private aggregate device. Spawned by the Rust core
// (`audio::system` / `audio::tap`); stopped with SIGTERM, which flushes + closes the WAV.
//
// This is the PREMIUM path; the ScreenCaptureKit `sysaudio` helper is the 13–14.3 fallback.
//
// Usage:  audiocap <output.wav> [maxSeconds]
// Exit:   0 ok · 2 bad args · 3 no permission / tap or aggregate failed · 4 unsupported OS
//
// ⚠️ RUNTIME-UNVERIFIED headless: real capture needs the Audio-Recording (TCC) grant + a live
// desktop session + actual system audio. Compilation (swiftc against the SDK) IS verified; the
// CATapDescription exclude-arg type (AudioObjectIDs, not PIDs) is confirmed from the SDK header.

import AVFoundation
import CoreAudio
import Foundation

// ── args ─────────────────────────────────────────────────────────────────────
let args = CommandLine.arguments
guard args.count >= 2 else {
    FileHandle.standardError.write(Data("usage: audiocap <output.wav> [maxSeconds]\n".utf8))
    exit(2)
}
let outURL = URL(fileURLWithPath: args[1])
// Wall-clock self-cap. An explicit `maxSeconds` argument wins; absent / unparsable / ≤ 0 falls
// back to a DEFAULT 4h cap (mirrors `MAX_RECORDING_SECONDS`, audio/recorder.rs) — NEVER uncapped:
// an uncapped orphan means unbounded disk writes (a real one outlived its parent by 7h20m and
// wrote 2+ GB of dead-session audio).
let requestedMaxSeconds: Double = args.count >= 3 ? (Double(args[2]) ?? 0) : 0
let maxSeconds: Double = requestedMaxSeconds > 0 ? requestedMaxSeconds : 4 * 60 * 60

guard #available(macOS 14.4, *) else {
    FileHandle.standardError.write(Data("audiocap: requires macOS 14.4+\n".utf8))
    exit(4)
}

// ── Core Audio helpers ────────────────────────────────────────────────────────

/// Translate a BSD process id to its Core Audio process AudioObjectID (0 on failure).
@available(macOS 14.4, *)
func processObjectID(for pid: pid_t) -> AudioObjectID {
    var pidValue = pid
    var address = AudioObjectPropertyAddress(
        mSelector: kAudioHardwarePropertyTranslatePIDToProcessObject,
        mScope: kAudioObjectPropertyScopeGlobal,
        mElement: kAudioObjectPropertyElementMain)
    var objectID = AudioObjectID(0)
    var size = UInt32(MemoryLayout<AudioObjectID>.size)
    let status = withUnsafeMutablePointer(to: &pidValue) { pidPtr -> OSStatus in
        AudioObjectGetPropertyData(
            AudioObjectID(kAudioObjectSystemObject), &address,
            UInt32(MemoryLayout<pid_t>.size), pidPtr, &size, &objectID)
    }
    return status == noErr ? objectID : 0
}

/// Read the tap's stream format (an ASBD) from `kAudioTapPropertyFormat`.
@available(macOS 14.4, *)
func tapStreamFormat(_ tapID: AudioObjectID) -> AudioStreamBasicDescription? {
    var address = AudioObjectPropertyAddress(
        mSelector: kAudioTapPropertyFormat,
        mScope: kAudioObjectPropertyScopeGlobal,
        mElement: kAudioObjectPropertyElementMain)
    var asbd = AudioStreamBasicDescription()
    var size = UInt32(MemoryLayout<AudioStreamBasicDescription>.size)
    let status = AudioObjectGetPropertyData(tapID, &address, 0, nil, &size, &asbd)
    return status == noErr ? asbd : nil
}

// ── capturer ──────────────────────────────────────────────────────────────────

@available(macOS 14.4, *)
final class TapCapturer {
    private let outURL: URL
    private let lock = NSLock()
    private var file: AVAudioFile?

    private var tapID = AudioObjectID(0)
    private var aggregateID = AudioObjectID(0)
    private var ioProcID: AudioDeviceIOProcID?

    // All-zero watchdog: count consecutive ~silent callbacks while the IOProc is live.
    private var silentCallbacks = 0

    init(outURL: URL) { self.outURL = outURL }

    private func setupAudio() throws {
        // Global-minus-self: exclude OUR app (the parent Murmur process) and this helper itself.
        // The Swift overlay refines the ObjC `NSArray<NSNumber*>` to `[AudioObjectID]` (confirmed
        // by the compiler), so pass the translated process object IDs directly.
        let excludePids = [getppid(), getpid()]
        let excludeObjects: [AudioObjectID] =
            excludePids
            .map { processObjectID(for: $0) }
            .filter { $0 != 0 }

        let tapDescription = CATapDescription(
            stereoGlobalTapButExcludeProcesses: excludeObjects)
        tapDescription.uuid = UUID()
        // muteBehavior defaults to CATapUnmuted — the user keeps hearing the audio while we tap it.
        tapDescription.isPrivate = true
        tapDescription.name = "MurmurSystemTap"

        var newTap = AudioObjectID(0)
        let tapStatus = AudioHardwareCreateProcessTap(tapDescription, &newTap)
        guard tapStatus == noErr, newTap != 0 else {
            throw err("AudioHardwareCreateProcessTap failed (\(tapStatus)) — likely no Audio-Recording permission")
        }
        tapID = newTap

        guard let asbd = tapStreamFormat(tapID) else {
            throw err("could not read tap format")
        }
        var format = asbd
        guard let avFormat = AVAudioFormat(streamDescription: &format) else {
            throw err("unsupported tap format")
        }

        // Private aggregate device wrapping the tap (with drift compensation).
        let aggUID = UUID().uuidString
        let tapUID = tapDescription.uuid.uuidString
        let aggDescription: [String: Any] = [
            kAudioAggregateDeviceNameKey as String: "MurmurAggregate",
            kAudioAggregateDeviceUIDKey as String: aggUID,
            kAudioAggregateDeviceIsPrivateKey as String: true,
            kAudioAggregateDeviceIsStackedKey as String: false,
            kAudioAggregateDeviceTapAutoStartKey as String: true,
            kAudioAggregateDeviceTapListKey as String: [
                [
                    kAudioSubTapUIDKey as String: tapUID,
                    kAudioSubTapDriftCompensationKey as String: true,
                ]
            ],
        ]
        var newAgg = AudioObjectID(0)
        let aggStatus = AudioHardwareCreateAggregateDevice(
            aggDescription as CFDictionary, &newAgg)
        guard aggStatus == noErr, newAgg != 0 else {
            throw err("AudioHardwareCreateAggregateDevice failed (\(aggStatus))")
        }
        aggregateID = newAgg

        // IOProc: write each input buffer list to the WAV in the tap's native format.
        let ioStatus = AudioDeviceCreateIOProcIDWithBlock(
            &ioProcID, aggregateID, DispatchQueue(label: "murmur.audiocap.io")
        ) { [weak self] _, inInputData, _, _, _ in
            self?.handle(inInputData, format: avFormat)
        }
        guard ioStatus == noErr, ioProcID != nil else {
            throw err("AudioDeviceCreateIOProcIDWithBlock failed (\(ioStatus))")
        }

        let startStatus = AudioDeviceStart(aggregateID, ioProcID)
        guard startStatus == noErr else {
            throw err("AudioDeviceStart failed (\(startStatus))")
        }
    }

    /// Public entry: set up the tap + aggregate + IOProc and begin capturing.
    func start() throws {
        try setupAudio()
        FileHandle.standardError.write(Data("audiocap: capturing\n".utf8))
    }

    /// Tear down only the Core Audio objects (tap, aggregate, IOProc) — leaves the open WAV file
    /// untouched so a watchdog rebuild keeps appending to the SAME recording (no audio lost).
    private func teardownAudio() {
        if let proc = ioProcID {
            AudioDeviceStop(aggregateID, proc)
            AudioDeviceDestroyIOProcID(aggregateID, proc)
            ioProcID = nil
        }
        if aggregateID != 0 {
            AudioHardwareDestroyAggregateDevice(aggregateID)
            aggregateID = 0
        }
        if tapID != 0 {
            AudioHardwareDestroyProcessTap(tapID)
            tapID = 0
        }
    }

    /// Rebuild the tap + aggregate — the only reliable recovery from the all-zero-buffer bug —
    /// keeping the same output file so no audio is lost across the rebuild.
    func rebuild() {
        teardownAudio()
        lock.lock()
        silentCallbacks = 0
        lock.unlock()
        try? setupAudio()
    }

    private func handle(_ bufferList: UnsafePointer<AudioBufferList>, format: AVAudioFormat) {
        guard let pcm = AVAudioPCMBuffer(pcmFormat: format, bufferListNoCopy: bufferList)
        else { return }

        // Watchdog accounting (peak across the buffer); see note in `start`.
        if let ch = pcm.floatChannelData {
            let frames = Int(pcm.frameLength)
            let channels = Int(format.channelCount)
            var peak: Float = 0
            for c in 0..<channels {
                let p = ch[c]
                for i in 0..<frames { peak = max(peak, abs(p[i])) }
            }
            silentCallbacks = peak < 1e-6 ? silentCallbacks + 1 : 0
        }

        lock.lock()
        defer { lock.unlock() }
        if file == nil {
            // Anchor line for the Rust wall-clock merge (true capture start). Fires once per
            // file; a watchdog rebuild re-enters with file != nil, so no duplicate line.
            FileHandle.standardError.write(Data("audiocap: first-frame\n".utf8))
            file = try? AVAudioFile(
                forWriting: outURL, settings: format.settings,
                commonFormat: .pcmFormatFloat32, interleaved: format.isInterleaved)
        }
        try? file?.write(from: pcm)
    }

    /// True if the tap has delivered only digital silence for a sustained run while live — the
    /// known "all-zero buffer" bug (Apple forum 825780); the caller rebuilds the tap+aggregate.
    func isStuckSilent(thresholdCallbacks: Int) -> Bool {
        lock.lock()
        defer { lock.unlock() }
        return silentCallbacks >= thresholdCallbacks
    }

    func stop() {
        teardownAudio()
        lock.lock()
        file = nil  // releasing the AVAudioFile flushes + closes it
        lock.unlock()
    }

    private func err(_ msg: String) -> NSError {
        NSError(domain: "murmur.audiocap", code: 3, userInfo: [NSLocalizedDescriptionKey: msg])
    }
}

// ── run ─────────────────────────────────────────────────────────────────────
let capturer = TapCapturer(outURL: outURL)

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

let sigQueue = DispatchQueue(label: "murmur.audiocap.sig")
for sig in [SIGINT, SIGTERM] {
    signal(sig, SIG_IGN)
    let src = DispatchSource.makeSignalSource(signal: sig, queue: sigQueue)
    src.setEventHandler { requestStop(0) }
    src.resume()
    _ = Unmanaged.passRetained(src)  // keep alive for process lifetime
}

// Parent-liveness watchdog: this helper must never outlive the Murmur process that spawned it.
// The parent normally SIGTERMs us on Stop/quit — but a SIGKILL'd / crashed / hot-rebuilt parent
// sends nothing, and the orphan then keeps capturing until the self-cap (the 7h20m incident).
// kqueue-backed EVFILT_PROC/NOTE_EXIT via DispatchSource — event-driven, no polling — routed into
// the SAME clean-stop path the signals use, so the WAV is flushed + closed before exit.
let parentPid = getppid()
// Already reparented to launchd (ppid 1) = the parent died while we were still launching. The
// Rust core always spawns this helper as a DIRECT child, so ppid 1 can only mean "orphaned before
// we could even observe the real parent" — watching launchd would wait forever (empirically
// verified: a parent that exits right after fork leaves the helper seeing ppid 1).
if parentPid == 1 { requestStop(0) }
let parentWatch = DispatchSource.makeProcessSource(
    identifier: parentPid, eventMask: .exit, queue: sigQueue)
parentWatch.setEventHandler { requestStop(0) }
parentWatch.resume()
_ = Unmanaged.passRetained(parentWatch)  // keep alive for process lifetime
// Close the registration race: if the parent died BEFORE the source was armed, its NOTE_EXIT
// already fired unseen and we were reparented (getppid() changed) — stop now instead of waiting
// on an event that will never come.
if getppid() != parentPid { requestStop(0) }

do {
    try capturer.start()
} catch {
    FileHandle.standardError.write(Data("audiocap: failed to start (\(error))\n".utf8))
    exit(3)
}

// All-zero watchdog: the tap can deliver only digital silence while the IOProc stays live (the
// Apple-unacknowledged bug, forum 825780); the only reliable recovery is a full tap+aggregate
// rebuild. CONSERVATIVE + bounded so it can't thrash on genuine meeting silence: only after a
// long sustained run of ~zero buffers, and at most a few times per recording. A false rebuild
// costs a sub-second gap; a missed real stuck-silent costs the whole "others" track — so we err
// toward rebuilding. THRESHOLDS ARE UNTUNED — they need a real Mac that exhibits the bug.
var rebuilds = 0
let watchdog = DispatchSource.makeTimerSource(queue: sigQueue)
watchdog.schedule(deadline: .now() + 15, repeating: 15)
watchdog.setEventHandler {
    if capturer.isStuckSilent(thresholdCallbacks: 1500), rebuilds < 2 {
        rebuilds += 1
        FileHandle.standardError.write(
            Data("audiocap: tap stuck silent — rebuilding (\(rebuilds))\n".utf8))
        capturer.rebuild()
    }
}
watchdog.resume()
_ = Unmanaged.passRetained(watchdog)

// Self-cap — ALWAYS armed (default 4h; see the `maxSeconds` derivation at the top).
// `wallDeadline` (not `deadline`): `DispatchTime` PAUSES while the machine sleeps, silently
// stretching the cap past its wall-clock intent — `DispatchWallTime` does not.
sigQueue.asyncAfter(wallDeadline: .now() + maxSeconds) { requestStop(0) }

RunLoop.main.run()
