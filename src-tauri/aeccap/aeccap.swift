// Murmur — AEC microphone-capture helper via AVAudioEngine VoiceProcessingIO.
//
// Captures the mic with the system VOICE PROCESSING (acoustic echo cancellation + noise
// suppression) enabled, into a WAV. Used as the ASR mic feed when the user records WITHOUT
// headphones (so the other participants' audio on the speakers is cancelled out of the mic). The
// raw cpal mic stays the archive source — this helper runs in PARALLEL and is purely best-effort.
//
// Usage:  aeccap <output.wav> [maxSeconds]
// Exit:   0 finalized · 2 bad args · 3 pre-ready/VPIO failure · 4 unsupported OS
//         5 finalized with capture I/O fault · 6 parent-loss hard bound (not finalized)
//
// ⚠️ RUNTIME-UNVERIFIED headless: whether this VPIO graph coexists with cpal on the same mic, and
// whether AEC actually cancels the echo, need a SIGNED build on a real Mac with a live call.
// Compilation and runtime verification remain PENDING for this exact tree.

import AVFoundation
import AudioToolbox
import Darwin
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

private let liveSrcOutputChunkFrames: AVAudioFrameCount = 4_096
private let captureQueueSlots = 32
private let captureQueueSlotFrames: AVAudioFrameCount = 16_384

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

    func enqueue(_ input: AVAudioPCMBuffer) {
        enqueue(
            input.mutableAudioBufferList, frameLength: input.frameLength,
            format: input.format)
    }

    func enqueue(
        _ inputList: UnsafePointer<AudioBufferList>, frameLength: AVAudioFrameCount,
        format: AVAudioFormat
    ) {
        // Fail closed on the first rejected callback. Resuming after overflow would concatenate
        // audio across an unknown middle gap and make the later track timeline incorrect.
        guard OSAtomicAdd32Barrier(0, &overflowed) == 0 else { return }
        guard OSAtomicAdd32Barrier(0, &closed) == 0,
            frameLength <= captureQueueSlotFrames
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
        let source = UnsafeMutableAudioBufferListPointer(
            UnsafeMutablePointer(mutating: inputList))
        let target = UnsafeMutableAudioBufferListPointer(slot.mutableAudioBufferList)
        let bytesPerFrame = Int(format.streamDescription.pointee.mBytesPerFrame)
        let expectedBytes = Int(frameLength) * bytesPerFrame
        guard source.count == target.count, bytesPerFrame > 0, frameLength > 0 else {
            OSAtomicCompareAndSwap32Barrier(0, 1, &overflowed)
            return
        }
        for index in 0..<source.count {
            let bytes = Int(source[index].mDataByteSize)
            let capacity = Int(slot.frameCapacity) * bytesPerFrame
            target[index].mDataByteSize = UInt32(capacity)
            guard let from = source[index].mData, let to = target[index].mData,
                bytes == expectedBytes, bytes % bytesPerFrame == 0, bytes <= capacity
            else {
                OSAtomicCompareAndSwap32Barrier(0, 1, &overflowed)
                return
            }
            memcpy(to, from, bytes)
            target[index].mDataByteSize = UInt32(bytes)
        }
        slot.frameLength = frameLength
        if !OSAtomicCompareAndSwap32Barrier(write, next, &writeIndex) {
            _ = OSAtomicCompareAndSwap32Barrier(0, 1, &overflowed)
        }
    }

    func consume(_ body: (AVAudioPCMBuffer) -> Void) -> Bool {
        let read = OSAtomicAdd32Barrier(0, &readIndex)
        let write = OSAtomicAdd32Barrier(0, &writeIndex)
        guard read != write else { return false }
        body(slots[Int(read)])
        _ = OSAtomicCompareAndSwap32Barrier(
            read, (read + 1) % Int32(captureQueueSlots), &readIndex)
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
    queue.enqueue(input)
    var exact = false
    guard queue.consume({ copied in
        exact = copied.frameLength == 128
            && copied.floatChannelData?[0][127] == samples[127]
    }), exact else { return false }
    for _ in 0..<captureQueueSlots { queue.enqueue(input) }
    guard queue.didOverflow else { return false }
    while queue.consume({ _ in }) {}
    queue.enqueue(input)
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
    partialQueue.enqueue(partialList, frameLength: partial.frameLength, format: format)
    mismatch.frameLength = 16
    let mismatchList = mismatch.mutableAudioBufferList
    let mismatchBuffers = UnsafeMutableAudioBufferListPointer(mismatchList)
    mismatchBuffers[1].mDataByteSize -= UInt32(MemoryLayout<Float>.size)
    mismatchQueue.enqueue(mismatchList, frameLength: mismatch.frameLength, format: format)
    let partialEmpty = !partialQueue.consume({ _ in })
    let mismatchEmpty = !mismatchQueue.consume({ _ in })
    return partialQueue.didOverflow && mismatchQueue.didOverflow
        && partialEmpty && mismatchEmpty
}

private func monoSourceFormat(_ format: AVAudioFormat) -> AVAudioFormat? {
    AVAudioFormat(commonFormat: .pcmFormatFloat32, sampleRate: format.sampleRate,
        channels: 1, interleaved: false)
}

/// Copy one bounded PCM frame range without assuming planar vs interleaved input. AudioConverter
/// can stop at its internal 4096-frame block even when handed a larger buffer; explicit slices make
/// consumption exact instead of silently losing the unconsumed suffix.
@available(macOS 10.15, *)
private func pcmSlice(
    _ input: AVAudioPCMBuffer, start: AVAudioFramePosition, frames: AVAudioFrameCount
) throws -> AVAudioPCMBuffer {
    guard let slice = AVAudioPCMBuffer(pcmFormat: input.format, frameCapacity: frames) else {
        throw NSError(
            domain: "murmur.aeccap", code: 5,
            userInfo: [NSLocalizedDescriptionKey: "could not allocate live SRC input slice"])
    }
    slice.frameLength = frames
    let sourceBuffers = UnsafeMutableAudioBufferListPointer(input.mutableAudioBufferList)
    let targetBuffers = UnsafeMutableAudioBufferListPointer(slice.mutableAudioBufferList)
    guard sourceBuffers.count == targetBuffers.count else {
        throw NSError(
            domain: "murmur.aeccap", code: 5,
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
                domain: "murmur.aeccap", code: 5,
                userInfo: [NSLocalizedDescriptionKey: "live SRC input slice is out of bounds"])
        }
        memcpy(target, source.advanced(by: byteOffset), byteCount)
        targetBuffers[index].mDataByteSize = UInt32(byteCount)
    }
    return slice
}

@available(macOS 10.15, *)
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
    else { throw NSError(domain: "murmur.aeccap", code: 5) }
    mono.frameLength = input.frameLength
    let stride = Int(input.stride)
    guard stride > 0 else { throw NSError(domain: "murmur.aeccap", code: 5) }
    let channels = Int(format.channelCount)
    for frame in 0..<Int(input.frameLength) {
        var sum: Float = 0
        for channel in 0..<channels {
            let value = data[format.isInterleaved ? 0 : channel][
                frame * stride + (format.isInterleaved ? channel : 0)]
            guard value.isFinite else { throw NSError(domain: "murmur.aeccap", code: 5) }
            sum += value
        }
        let value = sum / Float(channels)
        guard value.isFinite else { throw NSError(domain: "murmur.aeccap", code: 5) }
        output[frame] = value
    }
    return mono
}

@available(macOS 10.15, *)
private func submitLiveChunk(
    _ input: AVAudioPCMBuffer, using converter: AVAudioConverter?,
    to outputFormat: AVAudioFormat, consume: (AVAudioPCMBuffer) throws -> Void
) throws {
    let mono = try downmixFloat32ToMonoAtSourceRate(input)
    if mono.format.sampleRate == outputFormat.sampleRate {
        if mono.frameLength > 0 { try consume(mono) }
        return
    }
    guard let converter else { throw NSError(domain: "murmur.aeccap", code: 5) }
    var offset: AVAudioFramePosition = 0
    var retainedSlice: AVAudioPCMBuffer?
    while true {
        guard let output = AVAudioPCMBuffer(pcmFormat: outputFormat,
            frameCapacity: liveSrcOutputChunkFrames) else {
            throw NSError(domain: "murmur.aeccap", code: 5)
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
            throw conversionError ?? NSError(domain: "murmur.aeccap", code: 5)
        }
        if output.frameLength > 0 { try consume(output) }
        if status == .inputRanDry {
            guard offset == AVAudioFramePosition(mono.frameLength) else {
                throw NSError(domain: "murmur.aeccap", code: 5)
            }
            return
        }
        if status == .endOfStream || output.frameLength == 0 {
            throw NSError(domain: "murmur.aeccap", code: 5)
        }
    }
}

@available(macOS 10.15, *)
private func convertSubmittedChunk(
    _ input: AVAudioPCMBuffer, using converter: AVAudioConverter,
    to outputFormat: AVAudioFormat
) throws -> AVAudioPCMBuffer {
    guard input.format.commonFormat == .pcmFormatFloat32,
        input.format.channelCount == 1, !input.format.isInterleaved
    else {
        throw NSError(domain: "murmur.aeccap", code: 5,
            userInfo: [NSLocalizedDescriptionKey: "SRC oracle input must be planar mono float32"])
    }
    let ratio = outputFormat.sampleRate / input.format.sampleRate
    let capacity = ceil(Double(input.frameLength) * ratio) + 64
    guard capacity.isFinite, capacity > 0, capacity <= Double(UInt32.max),
        let output = AVAudioPCMBuffer(
            pcmFormat: outputFormat, frameCapacity: AVAudioFrameCount(capacity))
    else {
        throw NSError(domain: "murmur.aeccap", code: 5,
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
        throw conversionError ?? NSError(domain: "murmur.aeccap", code: 5,
            userInfo: [NSLocalizedDescriptionKey: "SRC oracle conversion failed"])
    }
    guard cursor == AVAudioFramePosition(input.frameLength), status == .inputRanDry else {
        throw NSError(domain: "murmur.aeccap", code: 5,
            userInfo: [NSLocalizedDescriptionKey: "SRC oracle did not consume input exactly once"])
    }
    return output
}

@available(macOS 10.15, *)
private func drainConverter(
    _ converter: AVAudioConverter, to outputFormat: AVAudioFormat,
    consume: (AVAudioPCMBuffer) throws -> Void
) throws {
    while true {
        guard let output = AVAudioPCMBuffer(
            pcmFormat: outputFormat, frameCapacity: liveSrcOutputChunkFrames)
        else { throw NSError(domain: "murmur.aeccap", code: 5) }
        var conversionError: NSError?
        let status = converter.convert(to: output, error: &conversionError) { _, inputStatus in
            inputStatus.pointee = .endOfStream
            return nil
        }
        if status == .error {
            throw conversionError ?? NSError(domain: "murmur.aeccap", code: 5,
                userInfo: [NSLocalizedDescriptionKey: "live SRC drain failed"])
        }
        if output.frameLength > 0 { try consume(output) }
        if status == .endOfStream { return }
        if status == .inputRanDry || (status == .haveData && output.frameLength == 0) {
            throw NSError(domain: "murmur.aeccap", code: 5,
                userInfo: [NSLocalizedDescriptionKey: "live SRC drain made no progress"])
        }
    }
}

@available(macOS 10.15, *)
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
                memcmp(sourceBytes.advanced(by: start * bytesPerFrame), targetBytes,
                    count * bytesPerFrame) == 0
            else { return false }
        }
    }
    return true
}

@available(macOS 10.15, *)
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

@available(macOS 10.15, *)
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
            throw NSError(domain: "murmur.aeccap", code: 5)
        }
        converter.primeMethod = .none
        var result: [Float] = []
        func append(_ buffer: AVAudioPCMBuffer) throws {
            guard let samples = buffer.floatChannelData?[0] else {
                throw NSError(domain: "murmur.aeccap", code: 5)
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
            throw NSError(domain: "murmur.aeccap", code: 5)
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
            "aeccap: exact SRC frame mismatch rate=\(inputRate) actual=\(chunked.count) reference=\(reference.count) expected=\(expectedFrames)\n".utf8))
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

/// General live PCM conversion, including sample-rate conversion. Apple's convenience
/// `convert(to:from:)` rejects SRC. Fixed input slices plus joined output ensure a large callback is
/// completely consumed, while `.none` priming avoids read-ahead across the live callback stream.
@available(macOS 10.15, *)
func convertLiveChunk(
    _ input: AVAudioPCMBuffer, using converter: AVAudioConverter,
    to outputFormat: AVAudioFormat
) throws -> AVAudioPCMBuffer {
    let mono = try downmixFloat32ToMonoAtSourceRate(input)
    let ratio = outputFormat.sampleRate / mono.format.sampleRate
    // Typical AVAudioEngine callbacks fit one converter output block; avoid input copying and
    // joined-output allocation on that hot path.
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
            domain: "murmur.aeccap", code: 5,
            userInfo: [NSLocalizedDescriptionKey: "invalid live SRC output capacity"])
    }

    guard outputFormat.commonFormat == .pcmFormatFloat32, outputFormat.channelCount == 1,
        let joinedSamples = joined.floatChannelData?[0]
    else {
        throw NSError(
            domain: "murmur.aeccap", code: 5,
            userInfo: [NSLocalizedDescriptionKey: "live SRC output must be mono float32"])
    }

    var sourceOffset: AVAudioFramePosition = 0
    var joinedFrames: AVAudioFrameCount = 0
    // Bound by OUTPUT frames, not input frames: 4096 input frames at 44.1→48 kHz would produce
    // ~4458 and hit AudioConverter's observed 4096-frame return ceiling.
    while sourceOffset < AVAudioFramePosition(mono.frameLength) {
        let remaining = AVAudioFrameCount(AVAudioFramePosition(mono.frameLength) - sourceOffset)
        let sliceFrames = min(remaining, maxInputFrames)
        let slice = try pcmSlice(mono, start: sourceOffset, frames: sliceFrames)
        let converted = try convertSubmittedChunk(slice, using: converter, to: outputFormat)
        guard let convertedSamples = converted.floatChannelData?[0],
            joinedFrames + converted.frameLength <= joined.frameCapacity
        else {
            throw NSError(
                domain: "murmur.aeccap", code: 5,
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

/// TCC-free executable regression for 96 kHz VPIO input. `meetnotes-aeccap --self-test-src` must
/// yield non-empty 48 kHz mono with the exact rational-duration output frame count.
@available(macOS 10.15, *)
func runSrcSmokeTest() -> Bool {
    let queueOk = verifyBoundedPcmQueue()
    let malformedQueueOk = verifyMalformedPcmQueueIsRejected()
    let planarSliceOk = verifyPcmSliceBoundaries(interleaved: false)
    let interleavedSliceOk = verifyPcmSliceBoundaries(interleaved: true)
    guard queueOk, malformedQueueOk, planarSliceOk, interleavedSliceOk else {
        FileHandle.standardError.write(Data(
            "aeccap: PCM boundary mismatch queue=\(queueOk) malformed=\(malformedQueueOk) planar=\(planarSliceOk) interleaved=\(interleavedSliceOk)\n".utf8))
        return false
    }
    for rate in [44_100.0, 96_000.0, 192_000.0] {
        for interleaved in [false, true] {
            do {
                guard try srcRegression(sampleRate: rate, interleaved: interleaved) else {
                    FileHandle.standardError.write(Data(
                        "aeccap: 9ch SRC regression mismatch rate=\(rate) layout=\(interleaved ? "interleaved" : "planar")\n".utf8))
                    return false
                }
            } catch {
                FileHandle.standardError.write(Data(
                    "aeccap: 9ch SRC regression error rate=\(rate) layout=\(interleaved ? "interleaved" : "planar"): \(error)\n".utf8))
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
        FileHandle.standardError.write(Data("aeccap: SRC self-test conversion error\n".utf8))
        return false
    }
    guard output.frameLength > 0, let mono = output.floatChannelData?[0] else {
        FileHandle.standardError.write(Data("aeccap: SRC self-test empty output\n".utf8))
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
                Data("aeccap: repeated SRC self-test conversion error\n".utf8))
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
                "aeccap: SRC self-test duration/energy mismatch (single=\(output.frameLength), repeated=\(repeatedFrames), upMono=\(upsampleMono.frames), upStereo=\(upsampleStereo.frames), upLarge=\(upsampleOversized.frames))\n"
                    .utf8))
    } else {
        FileHandle.standardError.write(
            Data(
                "aeccap: SRC self-test frames single=\(output.frameLength) repeated=\(repeatedFrames) upMono=\(upsampleMono.frames) upStereo=\(upsampleStereo.frames) upLarge=\(upsampleOversized.frames)\n"
                    .utf8))
    }
    return ok
}

if args[1] == "--self-test-src" {
    let ok = runSrcSmokeTest()
    FileHandle.standardError.write(Data("aeccap: SRC self-test \(ok ? "ok" : "failed")\n".utf8))
    exit(ok ? 0 : 5)
}

final class AecCapturer {
    private let outURL: URL
    private let engine = AVAudioEngine()
    private var file: AVAudioFile?
    private var monoFormat: AVAudioFormat?
    private var converter: AVAudioConverter?
    private var captureQueue: BoundedPcmQueue?
    private var writerGroup = DispatchGroup()
    private var srcFailureReported = false
    private var captureFailed: Int32 = 0

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
        // ALWAYS persist 48 kHz MONO float32. VPIO can hand us a MULTI-CHANNEL 96/192 kHz device
        // format (a 9-channel aggregate input was seen in the field): writing it verbatim ballooned
        // the WAV ~9x, while even mono 96 kHz float32 exceeds classic RIFF32 before Murmur's four-hour
        // cap. Bounded per-buffer conversion keeps the complete four-hour scratch ~2.76 GiB and the
        // downstream ASR feed duration-faithful regardless of the input device format.
        guard
            let mono = AVAudioFormat(
                commonFormat: .pcmFormatFloat32,
                sampleRate: 48_000,
                channels: 1,
                interleaved: false)
        else {
            throw NSError(
                domain: "murmur.aeccap", code: 3,
                userInfo: [NSLocalizedDescriptionKey: "could not construct 48 kHz mono format"])
        }
        monoFormat = mono
        if (tapFormat.channelCount != 1 || tapFormat.sampleRate != 48_000
            || tapFormat.isInterleaved || tapFormat.commonFormat != .pcmFormatFloat32)
        {
            guard let sourceMono = AVAudioFormat(commonFormat: .pcmFormatFloat32,
                sampleRate: tapFormat.sampleRate, channels: 1, interleaved: false) else {
                throw NSError(
                    domain: "murmur.aeccap", code: 3,
                    userInfo: [NSLocalizedDescriptionKey: "could not construct mono converter"])
            }
            if tapFormat.sampleRate != mono.sampleRate {
                guard let built = AVAudioConverter(from: sourceMono, to: mono) else {
                    throw NSError(domain: "murmur.aeccap", code: 3)
                }
                built.primeMethod = .none
                converter = built
            }
        }

        guard let captureQueue = BoundedPcmQueue(format: tapFormat) else {
            throw NSError(domain: "murmur.aeccap", code: 3,
                userInfo: [NSLocalizedDescriptionKey: "could not allocate bounded capture queue"])
        }
        self.captureQueue = captureQueue
        writerGroup = DispatchGroup()
        writerGroup.enter()
        DispatchQueue(label: "murmur.aeccap.writer", qos: .userInitiated).async {
            while true {
                let consumed = captureQueue.consume { [weak self] buffer in
                    self?.writeQueued(buffer)
                }
                if captureQueue.isClosedAndEmpty { break }
                if !consumed { Thread.sleep(forTimeInterval: 0.005) }
            }
            if captureQueue.didOverflow { self.latchCaptureFailure() }
            self.writerGroup.leave()
        }
        input.installTap(onBus: 0, bufferSize: 4096, format: tapFormat) { buffer, _ in
            captureQueue.enqueue(buffer)
        }
        engine.prepare()
        do {
            try engine.start()
        } catch {
            input.removeTap(onBus: 0)
            captureQueue.close()
            writerGroup.wait()
            throw error
        }
        FileHandle.standardError.write(
            Data(
                "aeccap: capturing (\(Int(tapFormat.sampleRate)) Hz, \(tapFormat.channelCount) ch in → 48 kHz mono out)\n"
                    .utf8))
    }

    private func writeQueued(_ buffer: AVAudioPCMBuffer) {
        guard OSAtomicAdd32Barrier(0, &captureFailed) == 0 else { return }
        guard let mono = monoFormat else { latchCaptureFailure(); return }
        do {
            try submitLiveChunk(buffer, using: converter, to: mono) { outBuffer in
                guard outBuffer.frameLength > 0 else { return }
                if self.file == nil {
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
                FileHandle.standardError.write(Data("aeccap: SRC/write failed: \(error)\n".utf8))
            }
        }
    }

    func stop() -> Bool {
        engine.inputNode.removeTap(onBus: 0)
        engine.stop()
        captureQueue?.close()
        writerGroup.wait()
        if let converter = converter, let monoFormat = monoFormat {
            do {
                try drainConverter(converter, to: monoFormat) { buffer in
                    guard let file = self.file else {
                        throw NSError(domain: "murmur.aeccap", code: 5,
                            userInfo: [NSLocalizedDescriptionKey:
                                "SRC drain produced output before the WAV was open"])
                    }
                    try file.write(from: buffer)
                }
            } catch {
                latchCaptureFailure()
                FileHandle.standardError.write(Data("aeccap: SRC drain/write failed: \(error)\n".utf8))
            }
        }
        file = nil  // releasing the AVAudioFile flushes + closes it
        let succeeded = OSAtomicAdd32Barrier(0, &captureFailed) == 0
        return succeeded
    }

    private func latchCaptureFailure() {
        _ = OSAtomicCompareAndSwap32Barrier(0, 1, &captureFailed)
    }
}

let capturer = AecCapturer(outURL: outURL)

// Signal, stdin EOF, and self-cap stops share this phase gate. VPIO teardown is only safe after
// `start()` returns; pre-ready requests therefore fail fast with NONZERO status, while a ready
// capture gets one clean flush/close and all later races are ignored.
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
        // A tiny callback can precede readiness. Avoid unsafe partial VPIO teardown and make Rust
        // reject any partial/unfinalized WAV; the raw cpal mic remains the authoritative fallback.
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

let sigQueue = DispatchQueue(label: "murmur.aeccap.sig")
for sig in [SIGINT, SIGTERM] {
    signal(sig, SIG_IGN)
    let src = DispatchSource.makeSignalSource(signal: sig, queue: sigQueue)
    src.setEventHandler { requestStop(0) }
    src.resume()
    _ = Unmanaged.passRetained(src)  // keep alive for the process lifetime
}

// Exact parent lifetime capability: Rust owns the only stdin writer. EOF identifies loss of this
// recorder owner without PID reuse, reparenting, or observer-registration races. Ignore bytes and
// keep blocking; retry EINTR, while EOF and every other read failure request a phase-safe stop.
// Stop runs on `sigQueue`; an independent 5 s wall-clock hard exit bounds a wedged VPIO finalizer.
// Exit 6 is not a finalized-file proof, so Rust never adopts a possibly partial WAV from that path.
DispatchQueue(label: "murmur.aeccap.parent-lifetime", qos: .utility).async {
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
    FileHandle.standardError.write(Data("aeccap: failed to start (\(error))\n".utf8))
    exit(3)
}

// Self-cap — ALWAYS armed (default 4h; see the `maxSeconds` derivation at the top).
// `wallDeadline` (not `deadline`): `DispatchTime` PAUSES while the machine sleeps, silently
// stretching the cap past its wall-clock intent — `DispatchWallTime` does not.
sigQueue.asyncAfter(wallDeadline: .now() + maxSeconds) { requestStop(0) }

RunLoop.main.run()
