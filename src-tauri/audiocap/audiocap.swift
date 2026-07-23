// Murmur — system-audio capture sidecar via the Core Audio PROCESS TAP API (macOS 14.4+).
//
// Captures the whole system audio output EXCEPT our own app (global-minus-self) into a WAV,
// using a process tap + a private aggregate device. Spawned by the Rust core
// (`audio::system` / `audio::tap`); stopped with SIGTERM, which flushes + closes the WAV.
//
// This is the PREMIUM path; the ScreenCaptureKit `sysaudio` helper is the 13–14.3 fallback.
//
// Usage:  audiocap <output.wav> [maxSeconds]
// Exit:   0 finalized · 2 bad args · 3 pre-ready failure · 4 unsupported OS
//         5 finalized with capture I/O fault · 6 parent-loss hard bound (not finalized)
//
// ⚠️ RUNTIME-UNVERIFIED headless: real capture needs the Audio-Recording (TCC) grant + a live
// desktop session + actual system audio. Compilation and the real TCC smoke test remain PENDING
// for this exact tree; the CATapDescription exclude-arg type is sourced from the SDK API.

import AVFoundation
import AudioToolbox
import CoreAudio
import Darwin
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

private let liveSrcOutputChunkFrames: AVAudioFrameCount = 4_096
private let captureQueueSlots = 32
private let captureQueueSlotFrames: AVAudioFrameCount = 16_384

/// Preallocated single-producer/single-consumer ring. The CoreAudio callback performs only bounds
/// checks, memcpy and atomic index publication; SRC, AVAudioFile, locks and logging belong to the
/// writer queue. OSAtomic is used deliberately here because Swift's standard library has no
/// dependency-free atomic primitive and adding a package to this tiny sidecar would widen supply
/// chain/runtime surface.
private final class BoundedPcmQueue {
    private let slots: [AVAudioPCMBuffer]
    private var writeIndex: Int32 = 0
    private var readIndex: Int32 = 0
    private var closed: Int32 = 0
    private var overflowed: Int32 = 0

    init?(format: AVAudioFormat) {
        var built: [AVAudioPCMBuffer] = []
        built.reserveCapacity(captureQueueSlots)
        for _ in 0..<captureQueueSlots {
            guard let slot = AVAudioPCMBuffer(
                pcmFormat: format, frameCapacity: captureQueueSlotFrames)
            else { return nil }
            built.append(slot)
        }
        slots = built
    }

    func enqueue(_ inputList: UnsafePointer<AudioBufferList>, format: AVAudioFormat) {
        // The first rejected callback defines the end of the trustworthy contiguous prefix. Never
        // resume after a slot frees: A,C with dropped B would shift every later timestamp.
        guard OSAtomicAdd32Barrier(0, &overflowed) == 0 else { return }
        let source = UnsafeMutableAudioBufferListPointer(
            UnsafeMutablePointer(mutating: inputList))
        let bytesPerFrame = Int(format.streamDescription.pointee.mBytesPerFrame)
        guard bytesPerFrame > 0, source.count > 0 else {
            OSAtomicCompareAndSwap32Barrier(0, 1, &overflowed)
            return
        }
        let firstBytes = Int(source[0].mDataByteSize)
        guard firstBytes > 0, firstBytes % bytesPerFrame == 0 else {
            OSAtomicCompareAndSwap32Barrier(0, 1, &overflowed)
            return
        }
        let frames = AVAudioFrameCount(firstBytes / bytesPerFrame)
        let expectedBytes = Int(frames) * bytesPerFrame
        guard OSAtomicAdd32Barrier(0, &closed) == 0,
            frames <= captureQueueSlotFrames
        else {
            OSAtomicCompareAndSwap32Barrier(0, 1, &overflowed)
            return
        }
        let write = OSAtomicAdd32Barrier(0, &writeIndex)
        let next = (write + 1) % Int32(captureQueueSlots)
        guard next != OSAtomicAdd32Barrier(0, &readIndex) else {
            OSAtomicCompareAndSwap32Barrier(0, 1, &overflowed)
            return
        }
        let slot = slots[Int(write)]
        let target = UnsafeMutableAudioBufferListPointer(slot.mutableAudioBufferList)
        guard source.count == target.count else {
            OSAtomicCompareAndSwap32Barrier(0, 1, &overflowed)
            return
        }
        for index in 0..<source.count {
            let bytes = Int(source[index].mDataByteSize)
            let capacity = Int(slot.frameCapacity) * bytesPerFrame
            target[index].mDataByteSize = UInt32(capacity)
            guard let from = source[index].mData, let to = target[index].mData,
                bytes == expectedBytes, bytes <= capacity
            else {
                OSAtomicCompareAndSwap32Barrier(0, 1, &overflowed)
                return
            }
            memcpy(to, from, bytes)
            target[index].mDataByteSize = UInt32(bytes)
        }
        slot.frameLength = frames
        if !OSAtomicCompareAndSwap32Barrier(write, next, &writeIndex) {
            _ = OSAtomicCompareAndSwap32Barrier(0, 1, &overflowed)
        }
    }

    func consume(_ body: (AVAudioPCMBuffer) -> Void) -> Bool {
        let read = OSAtomicAdd32Barrier(0, &readIndex)
        let write = OSAtomicAdd32Barrier(0, &writeIndex)
        guard read != write else { return false }
        body(slots[Int(read)])
        let next = (read + 1) % Int32(captureQueueSlots)
        _ = OSAtomicCompareAndSwap32Barrier(read, next, &readIndex)
        return true
    }

    func close() { _ = OSAtomicCompareAndSwap32Barrier(0, 1, &closed) }
    var isClosedAndEmpty: Bool {
        OSAtomicAdd32Barrier(0, &closed) != 0
            && OSAtomicAdd32Barrier(0, &readIndex) == OSAtomicAdd32Barrier(0, &writeIndex)
    }
    var didOverflow: Bool { OSAtomicAdd32Barrier(0, &overflowed) != 0 }
}

private func verifyBoundedPcmQueue() -> Bool {
    guard let format = AVAudioFormat(
        commonFormat: .pcmFormatFloat32, sampleRate: 48_000,
        channels: 1, interleaved: false),
        let queue = BoundedPcmQueue(format: format),
        let input = AVAudioPCMBuffer(pcmFormat: format, frameCapacity: 128),
        let samples = input.floatChannelData?[0]
    else { return false }
    input.frameLength = 128
    for index in 0..<128 { samples[index] = Float(index) / 128 }
    queue.enqueue(input.mutableAudioBufferList, format: format)
    var exact = false
    guard queue.consume({ copied in
        exact = copied.frameLength == 128
            && copied.floatChannelData?[0][127] == samples[127]
    }), exact else { return false }
    for _ in 0..<captureQueueSlots { queue.enqueue(input.mutableAudioBufferList, format: format) }
    guard queue.didOverflow else { return false }
    while queue.consume({ _ in }) {}
    queue.enqueue(input.mutableAudioBufferList, format: format)
    return !queue.consume({ _ in })
}

private func verifyMalformedPcmQueueIsRejected() -> Bool {
    guard let format = AVAudioFormat(
        commonFormat: .pcmFormatFloat32, sampleRate: 48_000,
        channels: 2, interleaved: false),
        let partialQueue = BoundedPcmQueue(format: format),
        let partial = AVAudioPCMBuffer(pcmFormat: format, frameCapacity: 16),
        let mismatchQueue = BoundedPcmQueue(format: format),
        let mismatch = AVAudioPCMBuffer(pcmFormat: format, frameCapacity: 16)
    else { return false }
    partial.frameLength = 16
    let partialList = partial.mutableAudioBufferList
    let partialBuffers = UnsafeMutableAudioBufferListPointer(partialList)
    partialBuffers[0].mDataByteSize -= 1
    partialQueue.enqueue(partialList, format: format)
    mismatch.frameLength = 16
    let mismatchList = mismatch.mutableAudioBufferList
    let mismatchBuffers = UnsafeMutableAudioBufferListPointer(mismatchList)
    mismatchBuffers[1].mDataByteSize -= UInt32(MemoryLayout<Float>.size)
    mismatchQueue.enqueue(mismatchList, format: format)
    let partialEmpty = !partialQueue.consume({ _ in })
    let mismatchEmpty = !mismatchQueue.consume({ _ in })
    return partialQueue.didOverflow && mismatchQueue.didOverflow
        && partialEmpty && mismatchEmpty
}

private func monoSourceFormat(_ format: AVAudioFormat) -> AVAudioFormat? {
    AVAudioFormat(commonFormat: .pcmFormatFloat32, sampleRate: format.sampleRate,
        channels: 1, interleaved: false)
}

/// Copy one frame range without assuming interleaved vs planar PCM. Keeping each submitted slice at
/// 4096 frames matters: AudioConverter may return after its internal block is full even when a
/// larger input buffer was supplied, silently discarding the unconsumed suffix if the caller treats
/// one `convert` call as all-consumed.
@available(macOS 14.4, *)
private func pcmSlice(
    _ input: AVAudioPCMBuffer, start: AVAudioFramePosition, frames: AVAudioFrameCount
) throws -> AVAudioPCMBuffer {
    guard let slice = AVAudioPCMBuffer(pcmFormat: input.format, frameCapacity: frames) else {
        throw NSError(
            domain: "murmur.audiocap", code: 5,
            userInfo: [NSLocalizedDescriptionKey: "could not allocate live SRC input slice"])
    }
    slice.frameLength = frames
    let sourceBuffers = UnsafeMutableAudioBufferListPointer(input.mutableAudioBufferList)
    let targetBuffers = UnsafeMutableAudioBufferListPointer(slice.mutableAudioBufferList)
    guard sourceBuffers.count == targetBuffers.count else {
        throw NSError(
            domain: "murmur.audiocap", code: 5,
            userInfo: [NSLocalizedDescriptionKey: "live SRC buffer layout changed"])
    }
    for index in 0..<sourceBuffers.count {
        // mDataByteSize is allocation-sized and may include tail padding. The ASBD stride is the
        // physical stride per AudioBuffer for both interleaved and non-interleaved PCM.
        let sourceByteSize = Int(sourceBuffers[index].mDataByteSize)
        let bytesPerFrame = Int(input.format.streamDescription.pointee.mBytesPerFrame)
        let byteOffset = Int(start) * bytesPerFrame
        let byteCount = Int(frames) * bytesPerFrame
        guard let source = sourceBuffers[index].mData, let target = targetBuffers[index].mData,
            bytesPerFrame > 0, byteOffset + byteCount <= sourceByteSize,
            byteCount <= Int(targetBuffers[index].mDataByteSize)
        else {
            throw NSError(
                domain: "murmur.audiocap", code: 5,
                userInfo: [NSLocalizedDescriptionKey: "live SRC input slice is out of bounds"])
        }
        memcpy(target, source.advanced(by: byteOffset), byteCount)
        targetBuffers[index].mDataByteSize = UInt32(byteCount)
    }
    return slice
}

@available(macOS 14.4, *)
private func downmixFloat32ToMonoAtSourceRate(
    _ input: AVAudioPCMBuffer
) throws -> AVAudioPCMBuffer {
    let format = input.format
    guard format.commonFormat == .pcmFormatFloat32, format.sampleRate.isFinite,
        format.sampleRate > 0, format.channelCount > 0, input.frameLength <= input.frameCapacity,
        let data = input.floatChannelData,
        let monoFormat = AVAudioFormat(commonFormat: .pcmFormatFloat32,
            sampleRate: format.sampleRate, channels: 1, interleaved: false),
        let mono = AVAudioPCMBuffer(pcmFormat: monoFormat, frameCapacity: input.frameLength),
        let output = mono.floatChannelData?[0]
    else {
        throw NSError(domain: "murmur.audiocap", code: 5,
            userInfo: [NSLocalizedDescriptionKey: "unsupported live SRC input format"])
    }
    mono.frameLength = input.frameLength
    let stride = Int(input.stride)
    guard stride > 0 else { throw NSError(domain: "murmur.audiocap", code: 5) }
    let channels = Int(format.channelCount)
    for frame in 0..<Int(input.frameLength) {
        var sum: Float = 0
        for channel in 0..<channels {
            let value = data[format.isInterleaved ? 0 : channel][
                frame * stride + (format.isInterleaved ? channel : 0)]
            guard value.isFinite else {
                throw NSError(domain: "murmur.audiocap", code: 5,
                    userInfo: [NSLocalizedDescriptionKey: "non-finite live SRC input"])
            }
            sum += value
        }
        let value = sum / Float(channels)
        guard value.isFinite else { throw NSError(domain: "murmur.audiocap", code: 5) }
        output[frame] = value
    }
    return mono
}

private func submitLiveChunk(
    _ input: AVAudioPCMBuffer, using converter: AVAudioConverter?,
    to outputFormat: AVAudioFormat, consume: (AVAudioPCMBuffer) throws -> Void
) throws {
    let mono = try downmixFloat32ToMonoAtSourceRate(input)
    if mono.format.sampleRate == outputFormat.sampleRate {
        if mono.frameLength > 0 { try consume(mono) }
        return
    }
    guard let converter else { throw NSError(domain: "murmur.audiocap", code: 5) }
    var offset: AVAudioFramePosition = 0
    var retainedSlice: AVAudioPCMBuffer?
    while true {
        guard let output = AVAudioPCMBuffer(pcmFormat: outputFormat,
            frameCapacity: liveSrcOutputChunkFrames) else {
            throw NSError(domain: "murmur.audiocap", code: 5)
        }
        var conversionError: NSError?
        let status = converter.convert(to: output, error: &conversionError) {
            requestedPackets, inputStatus in
            let remaining = AVAudioFramePosition(mono.frameLength) - offset
            guard remaining > 0 else {
                inputStatus.pointee = .noDataNow
                retainedSlice = nil
                return nil
            }
            let count = min(AVAudioFrameCount(remaining), max(1, requestedPackets))
            do {
                let exact = try pcmSlice(mono, start: offset, frames: count)
                retainedSlice = exact
                offset += AVAudioFramePosition(count)
                inputStatus.pointee = .haveData
                return exact
            } catch {
                inputStatus.pointee = .noDataNow
                return nil
            }
        }
        withExtendedLifetime(retainedSlice) {}
        if status == .error {
            throw conversionError ?? NSError(domain: "murmur.audiocap", code: 5)
        }
        if output.frameLength > 0 { try consume(output) }
        if status == .inputRanDry {
            guard offset == AVAudioFramePosition(mono.frameLength) else {
                throw NSError(domain: "murmur.audiocap", code: 5,
                    userInfo: [NSLocalizedDescriptionKey: "live SRC did not consume input"])
            }
            return
        }
        if status == .endOfStream {
            throw NSError(domain: "murmur.audiocap", code: 5,
                userInfo: [NSLocalizedDescriptionKey: "live SRC ended before drain"])
        }
        if output.frameLength == 0 {
            throw NSError(domain: "murmur.audiocap", code: 5,
                userInfo: [NSLocalizedDescriptionKey: "live SRC made no progress"])
        }
    }
}

// Independent test oracle for one already-downmixed mono buffer. Keep this separate from
// `submitLiveChunk`: the regression must prove the production downmix/streaming path against a
// converter input that begins with explicitly mono data, never the original multi-channel input.
@available(macOS 14.4, *)
private func convertSubmittedChunk(
    _ input: AVAudioPCMBuffer, using converter: AVAudioConverter,
    to outputFormat: AVAudioFormat
) throws -> AVAudioPCMBuffer {
    guard input.format.commonFormat == .pcmFormatFloat32,
        input.format.channelCount == 1, !input.format.isInterleaved
    else {
        throw NSError(domain: "murmur.audiocap", code: 5,
            userInfo: [NSLocalizedDescriptionKey: "SRC oracle input must be planar mono float32"])
    }
    let ratio = outputFormat.sampleRate / input.format.sampleRate
    let capacity = ceil(Double(input.frameLength) * ratio) + 64
    guard capacity.isFinite, capacity > 0, capacity <= Double(UInt32.max),
        let output = AVAudioPCMBuffer(
            pcmFormat: outputFormat, frameCapacity: AVAudioFrameCount(capacity))
    else {
        throw NSError(domain: "murmur.audiocap", code: 5,
            userInfo: [NSLocalizedDescriptionKey: "invalid SRC oracle capacity"])
    }
    var cursor: AVAudioFramePosition = 0
    var retainedSlice: AVAudioPCMBuffer?
    var conversionError: NSError?
    let status = converter.convert(to: output, error: &conversionError) {
        requestedPackets, inputStatus in
        let remaining = AVAudioFramePosition(input.frameLength) - cursor
        guard remaining > 0 else {
            inputStatus.pointee = .noDataNow
            retainedSlice = nil
            return nil
        }
        let count = min(AVAudioFrameCount(remaining), max(1, requestedPackets))
        do {
            let slice = try pcmSlice(input, start: cursor, frames: count)
            retainedSlice = slice
            cursor += AVAudioFramePosition(count)
            inputStatus.pointee = .haveData
            return slice
        } catch {
            inputStatus.pointee = .noDataNow
            return nil
        }
    }
    withExtendedLifetime(retainedSlice) {}
    if status == .error {
        throw conversionError ?? NSError(domain: "murmur.audiocap", code: 5,
            userInfo: [NSLocalizedDescriptionKey: "SRC oracle conversion failed"])
    }
    guard cursor == AVAudioFramePosition(input.frameLength), status == .inputRanDry else {
        throw NSError(domain: "murmur.audiocap", code: 5,
            userInfo: [NSLocalizedDescriptionKey: "SRC oracle did not consume input exactly once"])
    }
    return output
}

@available(macOS 14.4, *)
private func drainConverter(
    _ converter: AVAudioConverter, to outputFormat: AVAudioFormat,
    consume: (AVAudioPCMBuffer) throws -> Void
) throws {
    while true {
        guard let output = AVAudioPCMBuffer(
            pcmFormat: outputFormat, frameCapacity: liveSrcOutputChunkFrames)
        else { throw NSError(domain: "murmur.audiocap", code: 5) }
        var conversionError: NSError?
        let status = converter.convert(to: output, error: &conversionError) { _, inputStatus in
            inputStatus.pointee = .endOfStream
            return nil
        }
        if status == .error {
            throw conversionError ?? NSError(domain: "murmur.audiocap", code: 5,
                userInfo: [NSLocalizedDescriptionKey: "live SRC drain failed"])
        }
        if output.frameLength > 0 { try consume(output) }
        if status == .endOfStream { return }
        if status == .inputRanDry || (status == .haveData && output.frameLength == 0) {
            throw NSError(domain: "murmur.audiocap", code: 5,
                userInfo: [NSLocalizedDescriptionKey: "live SRC drain made no progress"])
        }
    }
}

@available(macOS 14.4, *)
private func verifyPcmSliceBoundaries(interleaved: Bool) -> Bool {
    guard let channelLayout = AVAudioChannelLayout(
        layoutTag: kAudioChannelLayoutTag_DiscreteInOrder | 9)
    else { return false }
    let format = AVAudioFormat(
        commonFormat: .pcmFormatFloat32, sampleRate: 48_000,
        interleaved: interleaved, channelLayout: channelLayout)
    guard let input = AVAudioPCMBuffer(pcmFormat: format, frameCapacity: 9_001) else {
        return false
    }
    input.frameLength = 9_001
    let source = UnsafeMutableAudioBufferListPointer(input.mutableAudioBufferList)
    for bufferIndex in 0..<source.count {
        guard let bytes = source[bufferIndex].mData?.assumingMemoryBound(to: UInt8.self) else {
            return false
        }
        for byte in 0..<Int(source[bufferIndex].mDataByteSize) {
            bytes[byte] = UInt8(truncatingIfNeeded: byte &* 29 &+ bufferIndex &* 71)
        }
    }
    let boundaries = [(0, 4_096), (4_096, 4_096), (8_192, 809),
                      (441, 4_410), (4_410, 4_410), (8_820, 181)]
    for (start, count) in boundaries {
        guard let slice = try? pcmSlice(
            input, start: AVAudioFramePosition(start), frames: AVAudioFrameCount(count))
        else { return false }
        let target = UnsafeMutableAudioBufferListPointer(slice.mutableAudioBufferList)
        guard target.count == source.count else { return false }
        for index in 0..<source.count {
            let bytesPerFrame = Int(format.streamDescription.pointee.mBytesPerFrame)
            guard let sourceBytes = source[index].mData, let targetBytes = target[index].mData,
                memcmp(
                    sourceBytes.advanced(by: start * bytesPerFrame), targetBytes,
                    count * bytesPerFrame) == 0
            else { return false }
        }
    }
    return true
}

@available(macOS 14.4, *)
private func hasExactSourceSentinels(_ samples: [Float]) -> Bool {
    let sentinelFrames = 256
    guard samples.count > sentinelFrames * 4 else { return false }
    func mean(_ range: Range<Int>) -> Double {
        range.reduce(0.0) { $0 + Double(samples[$1]) } / Double(range.count)
    }
    let first = mean(0..<sentinelFrames)
    let tail = mean((samples.count - sentinelFrames)..<samples.count)
    let guardStart = sentinelFrames * 2
    let guardEnd = samples.count - sentinelFrames * 2
    let guardMeanSquare = (guardStart..<guardEnd).reduce(0.0) {
        $0 + Double(samples[$1]) * Double(samples[$1])
    } / Double(guardEnd - guardStart)
    return abs(first - 0.25) <= 1e-6
        && abs(tail - (-0.375)) <= 1e-6
        && guardMeanSquare <= 1e-12
}

private func expectedSrcFrameCount(
    inputFrames: Int, inputRate: Int, outputRate: Int
) -> Int {
    precondition(inputFrames >= 0, "SRC frame expectation requires non-negative input frames")
    precondition(outputRate == 48_000, "SRC frame expectation is measured only at 48 kHz output")
    let unprimedFilterTailFramesByInputRate = [
        44_100: 33,
        96_000: 16,
        192_000: 7,
    ]
    guard let unprimedFilterTailFrames = unprimedFilterTailFramesByInputRate[inputRate] else {
        preconditionFailure("unsupported SRC self-test input rate: \(inputRate)")
    }
    let (numerator, multiplicationOverflow) = inputFrames.multipliedReportingOverflow(
        by: outputRate)
    precondition(!multiplicationOverflow, "SRC frame expectation multiplication overflow")
    let rationalDurationFrames = numerator / inputRate + (numerator % inputRate == 0 ? 0 : 1)
    let (expectedFrames, additionOverflow) = rationalDurationFrames.addingReportingOverflow(
        unprimedFilterTailFrames)
    precondition(!additionOverflow, "SRC frame expectation addition overflow")
    return expectedFrames
}

private func outputRetainsSentinels(
    _ samples: [Float], inputFrames: Int, inputRate: Int, outputRate: Int
) -> Bool {
    let sentinelFrames = 256
    let mappedSentinelFrames = max(
        16, expectedSrcFrameCount(
            inputFrames: sentinelFrames, inputRate: inputRate, outputRate: outputRate))
    guard samples.count > mappedSentinelFrames * 4 else { return false }
    func mean(_ range: Range<Int>) -> Double {
        range.reduce(0.0) { $0 + Double(samples[$1]) } / Double(range.count)
    }
    let firstMean = mean(0..<mappedSentinelFrames)
    let tailMean = mean((samples.count - mappedSentinelFrames)..<samples.count)
    let guardStart = mappedSentinelFrames * 2
    let guardEnd = samples.count - mappedSentinelFrames * 2
    let guardRms = sqrt((guardStart..<guardEnd).reduce(0.0) {
        $0 + Double(samples[$1]) * Double(samples[$1])
    } / Double(guardEnd - guardStart))
    // Signed, independently bounded windows prove location as well as retained energy. A
    // continuous tone cannot satisfy the low-energy region between them.
    return firstMean > 0.12 && tailMean < -0.18 && guardRms < 0.01
}

@available(macOS 14.4, *)
private func srcRegression(sampleRate: Double, interleaved: Bool) throws -> Bool {
    let frames = 9_001
    let sentinelFrames = 256
    let inputRate = Int(sampleRate)
    let outputRate = 48_000
    guard let channelLayout = AVAudioChannelLayout(
        layoutTag: kAudioChannelLayoutTag_DiscreteInOrder | 9)
    else { return false }
    let sourceFormat = AVAudioFormat(
        commonFormat: .pcmFormatFloat32, sampleRate: sampleRate,
        interleaved: interleaved, channelLayout: channelLayout)
    guard
        let outputFormat = AVAudioFormat(
            commonFormat: .pcmFormatFloat32, sampleRate: 48_000, channels: 1,
            interleaved: false),
        let source = AVAudioPCMBuffer(
            pcmFormat: sourceFormat, frameCapacity: AVAudioFrameCount(frames))
    else { return false }
    source.frameLength = AVAudioFrameCount(frames)
    guard let data = source.floatChannelData else { return false }
    let stride = Int(source.stride)
    for frame in 0..<frames {
        for channel in 0..<9 {
            let channelOffset = Float(channel - 4) * 0.01
            let value: Float
            if frame < sentinelFrames {
                value = 0.25 + channelOffset
            } else if frame >= frames - sentinelFrames {
                value = -0.375 + channelOffset
            } else {
                value = 0
            }
            data[interleaved ? 0 : channel][frame * stride + (interleaved ? channel : 0)] = value
        }
    }
    let explicitlyDownmixed = try downmixFloat32ToMonoAtSourceRate(source)
    guard let downmixedData = explicitlyDownmixed.floatChannelData?[0] else { return false }
    for frame in 0..<frames {
        var expected: Float = 0
        for channel in 0..<9 {
            expected += data[interleaved ? 0 : channel][
                frame * stride + (interleaved ? channel : 0)]
        }
        guard abs(downmixedData[frame] - expected / 9) <= Float.ulpOfOne * 16 else {
            return false
        }
    }
    let downmixed = Array(UnsafeBufferPointer(start: downmixedData, count: frames))
    guard hasExactSourceSentinels(downmixed) else { return false }
    var missingFirst = downmixed
    missingFirst.replaceSubrange(0..<sentinelFrames, with: repeatElement(0, count: sentinelFrames))
    var missingTail = downmixed
    missingTail.replaceSubrange(
        (frames - sentinelFrames)..<frames, with: repeatElement(0, count: sentinelFrames))
    guard !hasExactSourceSentinels(missingFirst), !hasExactSourceSentinels(missingTail) else {
        return false
    }

    func converted(_ chunks: [AVAudioPCMBuffer]) throws -> [Float] {
        guard let sourceMono = monoSourceFormat(sourceFormat),
            let converter = AVAudioConverter(from: sourceMono, to: outputFormat) else {
            throw NSError(domain: "murmur.audiocap", code: 5)
        }
        converter.primeMethod = .none
        var result: [Float] = []
        func append(_ buffer: AVAudioPCMBuffer) throws {
            guard let samples = buffer.floatChannelData?[0] else {
                throw NSError(domain: "murmur.audiocap", code: 5)
            }
            result.append(contentsOf: UnsafeBufferPointer(
                start: samples, count: Int(buffer.frameLength)))
        }
        for chunk in chunks {
            try submitLiveChunk(chunk, using: converter, to: outputFormat, consume: append)
        }
        try drainConverter(converter, to: outputFormat, consume: append)
        return result
    }

    guard let sourceMono = monoSourceFormat(sourceFormat),
        let referenceConverter = AVAudioConverter(from: sourceMono, to: outputFormat) else {
        return false
    }
    referenceConverter.primeMethod = .none
    var reference: [Float] = []
    func appendReference(_ buffer: AVAudioPCMBuffer) throws {
        guard let samples = buffer.floatChannelData?[0] else {
            throw NSError(domain: "murmur.audiocap", code: 5)
        }
        reference.append(contentsOf: UnsafeBufferPointer(
            start: samples, count: Int(buffer.frameLength)))
    }
    try appendReference(convertSubmittedChunk(
        explicitlyDownmixed, using: referenceConverter, to: outputFormat))
    try drainConverter(referenceConverter, to: outputFormat, consume: appendReference)
    var chunks: [AVAudioPCMBuffer] = []
    let pattern = [137, 3_971, 19, 2_003, 7, 1_111, 43]
    var offset = 0
    var patternIndex = 0
    while offset < frames {
        let count = min(pattern[patternIndex % pattern.count], frames - offset)
        chunks.append(try pcmSlice(source, start: AVAudioFramePosition(offset),
                                   frames: AVAudioFrameCount(count)))
        offset += count
        patternIndex += 1
    }
    let chunked = try converted(chunks)
    let expectedFrames = expectedSrcFrameCount(
        inputFrames: frames, inputRate: inputRate, outputRate: outputRate)
    guard reference.count == expectedFrames, chunked.count == expectedFrames else {
        FileHandle.standardError.write(Data(
            "audiocap: exact SRC frame mismatch rate=\(inputRate) actual=\(chunked.count) reference=\(reference.count) expected=\(expectedFrames)\n".utf8))
        return false
    }
    var maxError: Float = 0
    var squaredError: Double = 0
    for index in reference.indices {
        guard reference[index].isFinite, chunked[index].isFinite else { return false }
        let error = abs(reference[index] - chunked[index])
        maxError = max(maxError, error)
        squaredError += Double(error * error)
    }
    let rms = sqrt(squaredError / Double(reference.count))
    guard maxError <= 1e-4, rms <= 1e-5,
        outputRetainsSentinels(
            chunked, inputFrames: frames, inputRate: inputRate, outputRate: outputRate)
    else { return false }

    // AVAudioConverter's deterministic downmix must include every input channel.
    for hotChannel in 0..<9 {
        for frame in 0..<frames {
            for channel in 0..<9 {
                data[interleaved ? 0 : channel][frame * stride + (interleaved ? channel : 0)] =
                    channel == hotChannel && frame >= frames - 32 ? 0.5 : 0
            }
        }
        let oneHot = try converted([source])
        guard oneHot.reduce(Float(0), { max($0, abs($1)) }) > 0.001 else { return false }
    }
    return true
}

/// Convert one bounded live PCM callback with the general AVAudioConverter input-block API. The
/// convenience `convert(to:from:)` explicitly does NOT support sample-rate conversion. Input is
/// submitted in fixed slices and every result is appended, so callbacks larger than the converter's
/// internal 4096-frame block are neither truncated nor duplicated.
@available(macOS 14.4, *)
func convertLiveChunk(
    _ input: AVAudioPCMBuffer, using converter: AVAudioConverter,
    to outputFormat: AVAudioFormat
) throws -> AVAudioPCMBuffer {
    let mono = try downmixFloat32ToMonoAtSourceRate(input)
    let ratio = outputFormat.sampleRate / mono.format.sampleRate
    // Fast path for normal CoreAudio callbacks: one general conversion, no input copy and no
    // joined-output allocation. Only unusually large callbacks enter the slicing path below.
    let maxInputFrames = AVAudioFrameCount(
        max(1, floor(Double(liveSrcOutputChunkFrames) / max(ratio, 1))))
    if mono.frameLength <= maxInputFrames {
        return try convertSubmittedChunk(mono, using: converter, to: outputFormat)
    }
    let capacity = ceil(Double(mono.frameLength) * ratio) + 256
    guard capacity.isFinite, capacity > 0, capacity <= Double(UInt32.max),
        let joined = AVAudioPCMBuffer(
            pcmFormat: outputFormat, frameCapacity: AVAudioFrameCount(capacity))
    else {
        throw NSError(
            domain: "murmur.audiocap", code: 5,
            userInfo: [NSLocalizedDescriptionKey: "invalid live SRC output capacity"])
    }

    guard outputFormat.commonFormat == .pcmFormatFloat32, outputFormat.channelCount == 1,
        let joinedSamples = joined.floatChannelData?[0]
    else {
        throw NSError(
            domain: "murmur.audiocap", code: 5,
            userInfo: [NSLocalizedDescriptionKey: "live SRC output must be mono float32"])
    }

    var sourceOffset: AVAudioFramePosition = 0
    var joinedFrames: AVAudioFrameCount = 0
    // AudioConverter returned at most 4096 OUTPUT frames in the regression. For upsampling
    // (44.1→48 kHz), a fixed 4096-frame INPUT slice would itself produce ~4458 frames and be
    // truncated. Scale the input bound inversely so every submitted result fits one output block.
    while sourceOffset < AVAudioFramePosition(mono.frameLength) {
        let remaining = AVAudioFrameCount(AVAudioFramePosition(mono.frameLength) - sourceOffset)
        let sliceFrames = min(remaining, maxInputFrames)
        let slice = try pcmSlice(mono, start: sourceOffset, frames: sliceFrames)
        let converted = try convertSubmittedChunk(slice, using: converter, to: outputFormat)
        guard let convertedSamples = converted.floatChannelData?[0],
            joinedFrames + converted.frameLength <= joined.frameCapacity
        else {
            throw NSError(
                domain: "murmur.audiocap", code: 5,
                userInfo: [NSLocalizedDescriptionKey: "live SRC output exceeded its bound"])
        }
        memcpy(
            joinedSamples.advanced(by: Int(joinedFrames)), convertedSamples,
            Int(converted.frameLength) * MemoryLayout<Float>.size)
        joinedFrames += converted.frameLength
        sourceOffset += AVAudioFramePosition(sliceFrames)
    }
    joined.frameLength = joinedFrames
    return joined
}

/// Executable, TCC-free regression for the exact high-rate path that used to emit no audio. Build
/// the helper and run `meetnotes-audiocap --self-test-src`; success requires a non-empty 96→48 kHz
/// stereo→mono conversion with the exact rational-duration output frame count.
@available(macOS 14.4, *)
func runSrcSmokeTest() -> Bool {
    let queueOk = verifyBoundedPcmQueue()
    let malformedQueueOk = verifyMalformedPcmQueueIsRejected()
    let planarSliceOk = verifyPcmSliceBoundaries(interleaved: false)
    let interleavedSliceOk = verifyPcmSliceBoundaries(interleaved: true)
    guard queueOk, malformedQueueOk, planarSliceOk, interleavedSliceOk else {
        FileHandle.standardError.write(Data(
            "audiocap: PCM boundary mismatch queue=\(queueOk) malformed=\(malformedQueueOk) planar=\(planarSliceOk) interleaved=\(interleavedSliceOk)\n".utf8))
        return false
    }
    for rate in [44_100.0, 96_000.0, 192_000.0] {
        for interleaved in [false, true] {
            do {
                guard try srcRegression(sampleRate: rate, interleaved: interleaved) else {
                    FileHandle.standardError.write(Data(
                        "audiocap: 9ch SRC regression mismatch rate=\(rate) layout=\(interleaved ? "interleaved" : "planar")\n".utf8))
                    return false
                }
            } catch {
                FileHandle.standardError.write(Data(
                    "audiocap: 9ch SRC regression error rate=\(rate) layout=\(interleaved ? "interleaved" : "planar"): \(error)\n".utf8))
                return false
            }
        }
    }
    guard
        let inputFormat = AVAudioFormat(
            commonFormat: .pcmFormatFloat32, sampleRate: 96_000, channels: 2,
            interleaved: false),
        let outputFormat = AVAudioFormat(
            commonFormat: .pcmFormatFloat32, sampleRate: 48_000, channels: 1,
            interleaved: false)
    else { return false }
    guard
        let input = AVAudioPCMBuffer(pcmFormat: inputFormat, frameCapacity: 9_600),
        let channels = input.floatChannelData
    else { return false }
    guard
        let sourceMono = monoSourceFormat(inputFormat),
        let converter = AVAudioConverter(from: sourceMono, to: outputFormat)
    else { return false }
    input.frameLength = 9_600
    for frame in 0..<Int(input.frameLength) {
        let sample = sin(Float(frame) * 2 * Float.pi * 440 / 96_000) * 0.5
        channels[0][frame] = sample
        channels[1][frame] = sample
    }
    converter.primeMethod = .none
    let output: AVAudioPCMBuffer
    do {
        output = try convertLiveChunk(input, using: converter, to: outputFormat)
    } catch {
        FileHandle.standardError.write(Data("audiocap: SRC self-test conversion error\n".utf8))
        return false
    }
    guard output.frameLength > 0, let mono = output.floatChannelData?[0] else {
        FileHandle.standardError.write(Data("audiocap: SRC self-test empty output\n".utf8))
        return false
    }
    let duration = Double(output.frameLength) / outputFormat.sampleRate
    let peak = (0..<Int(output.frameLength)).reduce(Float(0)) {
        max($0, abs(mono[$1]))
    }
    let singleOk = abs(duration - 0.1) <= 0.005 && peak > 0.01
    guard let repeatedMono = monoSourceFormat(inputFormat),
        let repeatedConverter = AVAudioConverter(from: repeatedMono, to: outputFormat) else {
        return false
    }
    repeatedConverter.primeMethod = .none
    var repeatedFrames: UInt64 = 0
    var repeatedPeak: Float = 0
    for chunkIndex in 0..<20 {
        guard
            let chunk = AVAudioPCMBuffer(pcmFormat: inputFormat, frameCapacity: 960),
            let chunkChannels = chunk.floatChannelData
        else { return false }
        chunk.frameLength = 960
        for frame in 0..<Int(chunk.frameLength) {
            let absoluteFrame = chunkIndex * 960 + frame
            let sample = sin(Float(absoluteFrame) * 2 * Float.pi * 440 / 96_000) * 0.5
            chunkChannels[0][frame] = sample
            chunkChannels[1][frame] = sample
        }
        let converted: AVAudioPCMBuffer
        do {
            converted = try convertLiveChunk(
                chunk, using: repeatedConverter, to: outputFormat)
        } catch {
            FileHandle.standardError.write(
                Data("audiocap: repeated SRC self-test conversion error\n".utf8))
            return false
        }
        repeatedFrames += UInt64(converted.frameLength)
        if let samples = converted.floatChannelData?[0] {
            for frame in 0..<Int(converted.frameLength) {
                repeatedPeak = max(repeatedPeak, abs(samples[frame]))
            }
        }
    }

    func run441Case(channels channelCount: AVAudioChannelCount, framesPerChunk: Int, chunks: Int)
        -> (frames: UInt64, peak: Float)?
    {
        guard
            let sourceFormat = AVAudioFormat(
                commonFormat: .pcmFormatFloat32, sampleRate: 44_100,
                channels: channelCount, interleaved: false),
            let sourceMono = monoSourceFormat(sourceFormat),
            let upsampler = AVAudioConverter(from: sourceMono, to: outputFormat)
        else { return nil }
        upsampler.primeMethod = .none
        var totalFrames: UInt64 = 0
        var totalPeak: Float = 0
        for chunkIndex in 0..<chunks {
            guard
                let chunk = AVAudioPCMBuffer(
                    pcmFormat: sourceFormat,
                    frameCapacity: AVAudioFrameCount(framesPerChunk)),
                let channelData = chunk.floatChannelData
            else { return nil }
            chunk.frameLength = AVAudioFrameCount(framesPerChunk)
            for frame in 0..<framesPerChunk {
                let absoluteFrame = chunkIndex * framesPerChunk + frame
                let sample = sin(Float(absoluteFrame) * 2 * Float.pi * 440 / 44_100) * 0.5
                for channel in 0..<Int(channelCount) {
                    channelData[channel][frame] = sample
                }
            }
            guard
                let converted = try? convertLiveChunk(
                    chunk, using: upsampler, to: outputFormat),
                let samples = converted.floatChannelData?[0]
            else { return nil }
            totalFrames += UInt64(converted.frameLength)
            for frame in 0..<Int(converted.frameLength) {
                totalPeak = max(totalPeak, abs(samples[frame]))
            }
        }
        return (totalFrames, totalPeak)
    }

    guard
        let upsampleMono = run441Case(channels: 1, framesPerChunk: 441, chunks: 20),
        let upsampleStereo = run441Case(channels: 2, framesPerChunk: 441, chunks: 20),
        // One oversized callback forces the ratio-aware input slicer itself (8820 → ~9600).
        let upsampleOversized = run441Case(channels: 2, framesPerChunk: 8_820, chunks: 1)
    else { return false }
    let repeatedDuration = Double(repeatedFrames) / outputFormat.sampleRate
    let repeatedOk = abs(repeatedDuration - 0.2) <= 0.005 && repeatedPeak > 0.01
    let upsampleOk = [upsampleMono, upsampleStereo, upsampleOversized].allSatisfy {
        abs(Double($0.frames) / outputFormat.sampleRate - 0.2) <= 0.005 && $0.peak > 0.01
    }
    let ok = singleOk && repeatedOk && upsampleOk
    if !ok {
        FileHandle.standardError.write(
            Data(
                "audiocap: SRC self-test duration/energy mismatch (single=\(output.frameLength), repeated=\(repeatedFrames), upMono=\(upsampleMono.frames), upStereo=\(upsampleStereo.frames), upLarge=\(upsampleOversized.frames))\n"
                    .utf8))
    } else {
        FileHandle.standardError.write(
            Data(
                "audiocap: SRC self-test frames single=\(output.frameLength) repeated=\(repeatedFrames) upMono=\(upsampleMono.frames) upStereo=\(upsampleStereo.frames) upLarge=\(upsampleOversized.frames)\n"
                    .utf8))
    }
    return ok
}

if args[1] == "--self-test-src" {
    let ok = runSrcSmokeTest()
    FileHandle.standardError.write(Data("audiocap: SRC self-test \(ok ? "ok" : "failed")\n".utf8))
    exit(ok ? 0 : 5)
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
    private var file: AVAudioFile?

    private var tapID = AudioObjectID(0)
    private var aggregateID = AudioObjectID(0)
    private var ioProcID: AudioDeviceIOProcID?
    private var monoFormat: AVAudioFormat?
    private var monoConverter: AVAudioConverter?
    private var captureQueue: BoundedPcmQueue?
    private var writerGroup = DispatchGroup()

    // All-zero watchdog: count consecutive ~silent callbacks while the IOProc is live.
    private var silentCallbacks: Int32 = 0
    private var srcFailureReported = false
    private var captureFailed: Int32 = 0

    init(outURL: URL) { self.outURL = outURL }

    private func setupAudio() throws {
        // Global-minus-self: exclude OUR app (the parent Murmur process) and this helper itself.
        // `getppid()` is audio-filter metadata only; process lifetime is never inferred from it.
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
        // The process tap is normally stereo. Persisting that native stereo float32 stream hits
        // classic WAV's 4 GiB RIFF32 ceiling after ~3 h 06 min at 48 kHz — before Murmur's supported
        // four-hour recording cap — and AVAudioFile can then leave an overflowed/corrupt source.
        // Downmix + normalize to 48 kHz in the helper while each callback buffer is already
        // bounded. Merely making a 96/192 kHz output device mono would still overflow RIFF32 before
        // four hours; fixed 48 kHz mono float32 stays ~2.76 GiB for the full supported duration.
        guard
            let monoFormat = AVAudioFormat(
                commonFormat: .pcmFormatFloat32,
                sampleRate: 48_000,
                channels: 1,
                interleaved: false)
        else { throw err("could not construct mono tap format") }
        let needsMonoConversion =
            avFormat.channelCount != 1 || avFormat.isInterleaved
            || avFormat.commonFormat != .pcmFormatFloat32 || avFormat.sampleRate != 48_000
        let monoConverter: AVAudioConverter?
        if needsMonoConversion {
            guard let sourceMonoFormat = AVAudioFormat(commonFormat: .pcmFormatFloat32,
                sampleRate: avFormat.sampleRate, channels: 1, interleaved: false) else {
                throw err("could not construct tap mono converter")
            }
            if avFormat.sampleRate == monoFormat.sampleRate {
                monoConverter = nil
            } else {
                guard let converter = AVAudioConverter(from: sourceMonoFormat, to: monoFormat)
                else { throw err("could not construct tap mono converter") }
                converter.primeMethod = .none
                monoConverter = converter
            }
        } else {
            monoConverter = nil
        }
        self.monoFormat = monoFormat
        self.monoConverter = monoConverter
        guard let captureQueue = BoundedPcmQueue(format: avFormat) else {
            throw err("could not allocate bounded capture queue")
        }
        self.captureQueue = captureQueue

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

        // IOProc: downmix each bounded tap buffer to mono before appending to the WAV.
        let ioStatus = AudioDeviceCreateIOProcIDWithBlock(
            &ioProcID, aggregateID, DispatchQueue(label: "murmur.audiocap.io")
        ) { _, inInputData, _, _, _ in
            captureQueue.enqueue(inInputData, format: avFormat)
        }
        guard ioStatus == noErr, ioProcID != nil else {
            throw err("AudioDeviceCreateIOProcIDWithBlock failed (\(ioStatus))")
        }

        writerGroup = DispatchGroup()
        writerGroup.enter()
        DispatchQueue(label: "murmur.audiocap.writer", qos: .userInitiated).async {
            while true {
                let consumed = captureQueue.consume { [weak self] pcm in
                    self?.writeQueued(pcm, monoFormat: monoFormat, converter: monoConverter)
                }
                if captureQueue.isClosedAndEmpty { break }
                if !consumed { Thread.sleep(forTimeInterval: 0.005) }
            }
            if captureQueue.didOverflow { self.latchCaptureFailure() }
            self.writerGroup.leave()
        }

        let startStatus = AudioDeviceStart(aggregateID, ioProcID)
        guard startStatus == noErr else {
            captureQueue.close()
            writerGroup.wait()
            throw err("AudioDeviceStart failed (\(startStatus))")
        }
    }

    /// Public entry: set up the tap + aggregate + IOProc and begin capturing.
    func start() throws {
        try setupAudio()
        FileHandle.standardError.write(Data("audiocap: capturing (mono float32)\n".utf8))
    }

    /// Tear down only the Core Audio objects (tap, aggregate, IOProc) — leaves the open WAV file
    /// untouched so a watchdog rebuild keeps appending to the SAME recording (no audio lost).
    private func teardownAudio() {
        if let proc = ioProcID {
            AudioDeviceStop(aggregateID, proc)
            captureQueue?.close()
            writerGroup.wait()
            if let converter = monoConverter, let format = monoFormat {
                do {
                    try drainConverter(converter, to: format) { buffer in
                        guard let file = self.file else {
                            throw NSError(domain: "murmur.audiocap", code: 5,
                                userInfo: [NSLocalizedDescriptionKey:
                                    "SRC drain produced output before the WAV was open"])
                        }
                        try file.write(from: buffer)
                    }
                } catch {
                    latchCaptureFailure()
                    FileHandle.standardError.write(
                        Data("audiocap: SRC drain/write failed: \(error)\n".utf8))
                }
            }
            monoConverter = nil
            monoFormat = nil
            captureQueue = nil
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
        _ = OSAtomicCompareAndSwap32Barrier(
            OSAtomicAdd32Barrier(0, &silentCallbacks), 0, &silentCallbacks)
        do {
            try setupAudio()
        } catch {
            latchCaptureFailure()
            FileHandle.standardError.write(Data("audiocap: rebuild failed: \(error)\n".utf8))
        }
    }

    private func writeQueued(
        _ pcm: AVAudioPCMBuffer, monoFormat: AVAudioFormat,
        converter: AVAudioConverter?
    ) {
        // A write/SRC failure also terminates the exact prefix. Retrying later buffers would create
        // an unrepresented time gap even if the disk happened to recover.
        guard OSAtomicAdd32Barrier(0, &captureFailed) == 0 else { return }
        // Watchdog accounting belongs to the non-RT writer, so the IOProc performs no scan.
        if let ch = pcm.floatChannelData {
            let format = pcm.format
            let frames = Int(pcm.frameLength)
            let channels = Int(format.channelCount)
            var peak: Float = 0
            for c in 0..<channels {
                let p = ch[format.isInterleaved ? 0 : c]
                let channelOffset = format.isInterleaved ? c : 0
                for i in 0..<frames {
                    peak = max(peak, abs(p[i * Int(pcm.stride) + channelOffset]))
                }
            }
            let current = OSAtomicAdd32Barrier(0, &silentCallbacks)
            let next: Int32 = peak < 1e-6
                ? (current == Int32.max ? current : current + 1)
                : 0
            _ = OSAtomicCompareAndSwap32Barrier(current, next, &silentCallbacks)
        }
        do {
            try submitLiveChunk(pcm, using: converter, to: monoFormat) { outBuffer in
                guard outBuffer.frameLength > 0 else { return }
                if self.file == nil {
                    FileHandle.standardError.write(Data("audiocap: first-frame\n".utf8))
                    self.file = try AVAudioFile(forWriting: self.outURL,
                        settings: outBuffer.format.settings,
                        commonFormat: .pcmFormatFloat32, interleaved: false)
                }
                try self.file?.write(from: outBuffer)
            }
        } catch {
            latchCaptureFailure()
            if !srcFailureReported {
                srcFailureReported = true
                FileHandle.standardError.write(Data("audiocap: SRC/write failed: \(error)\n".utf8))
            }
        }
    }

    /// True if the tap has delivered only digital silence for a sustained run while live — the
    /// known "all-zero buffer" bug (Apple forum 825780); the caller rebuilds the tap+aggregate.
    func isStuckSilent(thresholdCallbacks: Int) -> Bool {
        OSAtomicAdd32Barrier(0, &silentCallbacks) >= Int32(thresholdCallbacks)
    }

    func stop() -> Bool {
        teardownAudio()
        file = nil  // releasing the AVAudioFile flushes + closes it
        let succeeded = OSAtomicAdd32Barrier(0, &captureFailed) == 0
        return succeeded
    }

    private func latchCaptureFailure() {
        _ = OSAtomicCompareAndSwap32Barrier(0, 1, &captureFailed)
    }

    private func err(_ msg: String) -> NSError {
        NSError(domain: "murmur.audiocap", code: 3, userInfo: [NSLocalizedDescriptionKey: msg])
    }
}

// ── run ─────────────────────────────────────────────────────────────────────
let capturer = TapCapturer(outURL: outURL)

// Signal, stdin EOF, and the wall cap all pass through one phase-aware/idempotent gate. Teardown
// touches Core Audio objects that may not exist until `start()` returns, so pre-ready stop requests
// fail fast with NONZERO status; a ready capture finalizes exactly once.
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
        // Tiny callbacks can precede readiness, but partial teardown is not synchronized here.
        // `_exit(3)` makes Rust reject any partial/unfinalized WAV and fall back to the raw mic.
        _exit(3)
    case .ready:
        capturePhase = .stopping
        stopLock.unlock()
        let succeeded = capturer.stop()
        exit(succeeded ? code : 5)
    }
}
func markCaptureReady() {
    stopLock.lock()
    if case .starting = capturePhase { capturePhase = .ready }
    stopLock.unlock()
}

let sigQueue = DispatchQueue(label: "murmur.audiocap.sig")
for sig in [SIGINT, SIGTERM] {
    signal(sig, SIG_IGN)
    let src = DispatchSource.makeSignalSource(signal: sig, queue: sigQueue)
    src.setEventHandler { requestStop(0) }
    src.resume()
    _ = Unmanaged.passRetained(src)  // keep alive for process lifetime
}

// Exact parent lifetime capability: Rust retains the only stdin writer for this recorder. A
// blocking read reaches EOF precisely when that owner disappears, with no PID-reuse/reparenting or
// DispatchSource registration race. Bytes are ignored (this is a lifetime capability, not a data
// protocol); EINTR retries, while EOF and every other error request a phase-safe stop. Stop is
// serialized on `sigQueue` with the all-zero rebuild watchdog. An independent 5 s wall-clock hard
// exit prevents a wedged rebuild/finalizer from becoming an orphan; exit 6 is intentionally not a
// finalized-file proof, so Rust will not adopt a possibly partial WAV.
DispatchQueue(label: "murmur.audiocap.parent-lifetime", qos: .utility).async {
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

do {
    try capturer.start()
    markCaptureReady()
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
