import AVFoundation

/// Speaker + microphone at the modem's format (48 kHz mono Float32).
///
/// The session uses `.measurement` mode so iOS does not apply automatic gain,
/// noise suppression or echo cancellation to the microphone — those would
/// destroy the modem signal.
/// Thread safety: all mutable state is touched only on `queue` (and the audio
/// render thread for the tap), so it is safe to share across concurrency domains.
final class AudioIO: @unchecked Sendable {
    let format = AVAudioFormat(commonFormat: .pcmFormatFloat32, sampleRate: Modem.sampleRate,
                               channels: 1, interleaved: false)!
    private let engine = AVAudioEngine()
    private let player = AVAudioPlayerNode()
    private var converter: AVAudioConverter?
    private var running = false
    /// All session/engine work happens here: activating the audio session can block
    /// for a noticeable time and must not run on the main thread.
    private let queue = DispatchQueue(label: "tonesend.audio", qos: .userInitiated)

    /// Called on the audio thread with converted mono samples.
    var onSamples: (([Float]) -> Void)?

    init() {
        let nc = NotificationCenter.default
        let log = EventLog.shared
        nc.addObserver(forName: AVAudioSession.routeChangeNotification, object: nil, queue: nil) { n in
            let reason = (n.userInfo?[AVAudioSessionRouteChangeReasonKey] as? UInt)
                .flatMap(AVAudioSession.RouteChangeReason.init(rawValue:))
            let name: String = switch reason {
            case .newDeviceAvailable: "new_device"
            case .oldDeviceUnavailable: "device_removed"
            case .categoryChange: "category_change"
            case .override: "override"
            case .wakeFromSleep: "wake"
            case .noSuitableRouteForCategory: "no_route"
            case .routeConfigurationChange: "configuration"
            default: "unknown"
            }
            log.log("audio_route_change", DeviceInfo.audioFields.merging(["reason": name]) { a, _ in a })
        }
        nc.addObserver(forName: AVAudioSession.interruptionNotification, object: nil, queue: nil) { n in
            let began = (n.userInfo?[AVAudioSessionInterruptionTypeKey] as? UInt) == AVAudioSession.InterruptionType.began.rawValue
            log.log("audio_interruption", ["began": began])
        }
        nc.addObserver(forName: AVAudioSession.mediaServicesWereResetNotification, object: nil, queue: nil) { _ in
            log.log("audio_media_services_reset")
        }
        nc.addObserver(forName: .AVAudioEngineConfigurationChange, object: engine, queue: nil) { _ in
            log.log("audio_engine_configuration_change")
        }
    }

    static func requestPermission() async -> Bool {
        await AVAudioApplication.requestRecordPermission()
    }

    /// Activate the session and start the engine (off the main thread).
    func start() async throws {
        try await withCheckedThrowingContinuation { (c: CheckedContinuation<Void, Error>) in
            queue.async {
                do {
                    try self.startOnQueue()
                    c.resume()
                } catch {
                    c.resume(throwing: error)
                }
            }
        }
    }

    private func startOnQueue() throws {
        guard !running else { return }
        let session = AVAudioSession.sharedInstance()
        try session.setCategory(.playAndRecord, mode: .measurement, options: [.defaultToSpeaker])
        try? session.setPreferredSampleRate(Modem.sampleRate)
        try? session.setPreferredIOBufferDuration(0.02)
        try session.setActive(true)

        if player.engine == nil {
            engine.attach(player)
            engine.connect(player, to: engine.mainMixerNode, format: format)
        }

        let input = engine.inputNode
        let inFormat = input.outputFormat(forBus: 0)
        converter = (inFormat.sampleRate == format.sampleRate && inFormat.channelCount == 1)
            ? nil : AVAudioConverter(from: inFormat, to: format)
        input.installTap(onBus: 0, bufferSize: 2048, format: inFormat) { [weak self] buffer, _ in
            self?.handle(buffer)
        }
        engine.prepare()
        try engine.start()
        running = true
        EventLog.shared.log("audio_engine_start", [
            "mic_sample_rate": inFormat.sampleRate, "mic_channels": inFormat.channelCount,
            "converting": converter != nil,
        ])
    }

    func stop() {
        queue.async { [self] in
            guard running else { return }
            engine.inputNode.removeTap(onBus: 0)
            player.stop()
            engine.stop()
            running = false
            try? AVAudioSession.sharedInstance().setActive(false, options: .notifyOthersOnDeactivation)
        }
    }

    private func handle(_ buffer: AVAudioPCMBuffer) {
        var out = buffer
        if let converter {
            let ratio = format.sampleRate / buffer.format.sampleRate
            let cap = AVAudioFrameCount(Double(buffer.frameLength) * ratio + 32)
            guard let converted = AVAudioPCMBuffer(pcmFormat: format, frameCapacity: cap) else { return }
            var fed = false
            var error: NSError?
            converter.convert(to: converted, error: &error) { _, status in
                if fed {
                    status.pointee = .noDataNow
                    return nil
                }
                fed = true
                status.pointee = .haveData
                return buffer
            }
            if error != nil { return }
            out = converted
        }
        guard let ch = out.floatChannelData?[0] else { return }
        onSamples?(Array(UnsafeBufferPointer(start: ch, count: Int(out.frameLength))))
    }

    /// Play mono samples; `completion` runs (on an audio thread) after playback.
    func play(_ samples: [Float], completion: @escaping () -> Void) {
        queue.async { [self] in playOnQueue(samples, completion: completion) }
    }

    private func playOnQueue(_ samples: [Float], completion: @escaping () -> Void) {
        guard running, !samples.isEmpty,
              let buf = AVAudioPCMBuffer(pcmFormat: format, frameCapacity: AVAudioFrameCount(samples.count))
        else { completion(); return }
        buf.frameLength = buf.frameCapacity
        samples.withUnsafeBufferPointer { src in
            buf.floatChannelData![0].update(from: src.baseAddress!, count: samples.count)
        }
        player.scheduleBuffer(buf, at: nil, options: [], completionCallbackType: .dataPlayedBack) { _ in
            completion()
        }
        if !player.isPlaying { player.play() }
    }

    func stopPlayback() {
        queue.async { [self] in player.stop() }
    }
}

enum WAV {
    /// 16-bit PCM mono WAV.
    static func data(_ samples: [Float], sampleRate: Int = Int(Modem.sampleRate)) -> Data {
        var d = Data()
        func u32(_ v: UInt32) { withUnsafeBytes(of: v.littleEndian) { d.append(contentsOf: $0) } }
        func u16(_ v: UInt16) { withUnsafeBytes(of: v.littleEndian) { d.append(contentsOf: $0) } }
        let bytes = UInt32(samples.count * 2)
        d.append(contentsOf: Array("RIFF".utf8)); u32(36 + bytes)
        d.append(contentsOf: Array("WAVEfmt ".utf8)); u32(16); u16(1); u16(1)
        u32(UInt32(sampleRate)); u32(UInt32(sampleRate * 2)); u16(2); u16(16)
        d.append(contentsOf: Array("data".utf8)); u32(bytes)
        d.reserveCapacity(d.count + samples.count * 2)
        for s in samples {
            u16(UInt16(bitPattern: Int16(max(-1, min(1, s)) * 32767)))
        }
        return d
    }
}
