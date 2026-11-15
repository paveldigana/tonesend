import Foundation
import SwiftUI

/// Receive pipeline living on its own serial queue (the decoder is not thread-safe).
private final class RxPipeline {
    let queue = DispatchQueue(label: "tonesend.dsp", qos: .userInitiated)
    private var receiver: ModemReceiver
    private var raw: [Float]?
    private var lastStatus = Date.distantPast
    /// Microphone level statistics, logged every `levelEvery` seconds while listening.
    private var levels: [Float] = []
    private var levelsStart = Date()
    private static let levelEvery: TimeInterval = 10
    /// While paused, samples are ignored but partially received transfers are kept,
    /// so repair data sent later still merges with them.
    var paused = true

    var onEvents: (([TransferEvent]) -> Void)?
    var onStatus: ((ReceiverStatus) -> Void)?

    init(bands: Set<BandProfile>) { receiver = ModemReceiver(bands: bands) }

    func push(_ samples: [Float]) {
        queue.async { [self] in
            raw?.append(contentsOf: samples)
            guard !paused else { return }
            let events = receiver.push(samples)
            if !events.isEmpty { onEvents?(events) }
            if let lines = receiver.takeLog() { EventLog.shared.logFramework(lines) }
            let now = Date()
            levels.append(receiver.status.levelDB)
            if now.timeIntervalSince(levelsStart) >= Self.levelEvery { flushLevels(now) }
            if now.timeIntervalSince(lastStatus) > 0.08 || !events.isEmpty {
                lastStatus = now
                onStatus?(receiver.status)
            }
        }
    }

    /// Feed a whole recording synchronously (demo mode).
    func decodeNow(_ samples: [Float]) -> [TransferEvent] {
        queue.sync {
            var out: [TransferEvent] = []
            stride(from: 0, to: samples.count, by: 1024).forEach { i in
                out += receiver.push(Array(samples[i..<min(i + 1024, samples.count)]))
            }
            if let lines = receiver.takeLog() { EventLog.shared.logFramework(lines) }
            return out
        }
    }

    func reset(bands: Set<BandProfile>) {
        queue.async { [self] in receiver = ModemReceiver(bands: bands) }
    }

    func setPaused(_ p: Bool) {
        queue.async { [self] in
            if p && !paused { flushLevels(Date()) }
            if !p && paused { levels = []; levelsStart = Date() }
            paused = p
        }
    }

    /// Log min / median / 90th percentile / max input level (dBFS) of the last period.
    private func flushLevels(_ now: Date) {
        defer { levels = []; levelsStart = now }
        guard levels.count >= 4 else { return }
        let s = levels.sorted()
        EventLog.shared.log("rx_level", [
            "period_s": now.timeIntervalSince(levelsStart),
            "min_db": s.first, "median_db": s[s.count / 2],
            "p90_db": s[s.count * 9 / 10], "max_db": s.last,
        ])
    }

    func startRaw() { queue.async { [self] in raw = [] } }

    func stopRaw(_ done: @escaping ([Float]) -> Void) {
        queue.async { [self] in
            let r = raw ?? []
            raw = nil
            done(r)
        }
    }
}

@MainActor
final class AppModel: ObservableObject {
    // Receive
    @Published var listening = false
    @Published var status = ReceiverStatus()
    @Published var received: [Transfer] = []
    @Published var micDenied = false
    @Published var listenBands: Set<BandProfile> = Set(BandProfile.allCases) {
        didSet {
            rx.reset(bands: listenBands)
            log.log("listen_bands", ["bands": listenBands.map(\.logName).sorted()])
        }
    }
    /// Most recent arrival (for a highlight).
    @Published var justReceived: Transfer.ID?

    // Send
    @Published var sent: [Transfer] = []
    @Published var sending: (transfer: Transfer, start: Date, duration: Double, repair: Bool)?
    @Published var lastError: String? {
        didSet { if let lastError { log.log("error", ["message": lastError]) } }
    }

    // Lab
    @Published var rawRecording = false
    @Published var rawRecordingStart: Date?
    @Published var rawFile: URL?
    @Published var probePlaying: Date?

    /// Extra repair data sent by one tap on "Send repair data".
    static let repairStep: Float = 0.3

    private let log = EventLog.shared
    private var listenStart: Date?
    private let audio = AudioIO()
    private let rx: RxPipeline
    private var audioRunning = false

    init() {
        rx = RxPipeline(bands: Set(BandProfile.allCases))
        audio.onSamples = { [rx] samples in rx.push(samples) }
        rx.onEvents = { [weak self] events in
            Task { @MainActor in self?.handle(events) }
        }
        rx.onStatus = { [weak self] s in
            Task { @MainActor in
                guard let self, self.listening else { return }
                if self.status != s { self.status = s }
            }
        }
        let demo = UserDefaults.standard.bool(forKey: "demo")
        log.log("app_start", DeviceInfo.fields.merging(["demo": demo]) { a, _ in a })
        if demo { loadDemo() }
    }

    /// Free-text note from the tester (distance, room, phone position…).
    func addNote(_ text: String) {
        let s = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !s.isEmpty else { return }
        log.log("note", ["text": s])
    }

    /// `-demo YES` launch argument: real encode → decode through the Rust modem
    /// (no audio hardware) so the UI has content and the FFI is exercised.
    private func loadDemo() {
        let items: [(TransferContent, BandProfile, RateProfile)] = [
            (.text("Hello from the other iPhone 👋 This message travelled as sound."), .wide, .fast),
            (.text("Silent mode test — 17.5 to 20.5 kHz."), .inaudible, .normal),
            (.text("BEGIN:VCARD\nVERSION:3.0\nN:Novák;Jan\nFN:Jan Novák\nTEL;TYPE=CELL:+420 777 123 456\nEMAIL:jan@example.cz\nURL:https://www.example.cz/\nEND:VCARD\n"), .max, .fast),
        ]
        for (i, (content, band, rate)) in items.enumerated() {
            var audio = [Float](repeating: 0, count: 4800)
            audio += Modem.encode(content.packed().0, band: band, rate: rate, id: UInt32(100 + i), redundancy: 0.25)
            audio += [Float](repeating: 0, count: 48000 * 4)
            handle(rx.decodeNow(audio))
        }
    }

    // MARK: audio lifecycle

    private func ensureAudio() async -> Bool {
        if audioRunning { return true }
        guard await AudioIO.requestPermission() else {
            micDenied = true
            log.log("mic_denied")
            return false
        }
        do {
            try await audio.start()
            audioRunning = true
            log.log("audio_start", DeviceInfo.audioFields)
            return true
        } catch {
            lastError = "Audio start failed: \(error.localizedDescription)"
            return false
        }
    }

    private func stopAudioIfIdle() {
        if !listening && sending == nil && !rawRecording && probePlaying == nil && audioRunning {
            audio.stop()
            audioRunning = false
            log.log("audio_stop")
        }
    }

    /// Our own speaker would be decoded too, so ignore the mic while playing.
    private var playing: Bool { sending != nil || probePlaying != nil }

    private func updateRxPause() {
        rx.setPaused(!listening || playing)
    }

    // MARK: receive

    func toggleListening() {
        Task {
            if listening {
                listening = false
                log.log("listen_stop", ["duration_s": listenStart.map { Date().timeIntervalSince($0) }])
                listenStart = nil
                updateRxPause()
                status = ReceiverStatus()
                stopAudioIfIdle()
            } else if await ensureAudio() {
                listening = true
                listenStart = Date()
                log.log("listen_start", ["bands": listenBands.map(\.logName).sorted()])
                updateRxPause()
            }
        }
    }

    private func logEvent(_ e: TransferEvent, content: TransferContent?, compressed: Bool) {
        let s = e.stats
        let type = e.repeatCount != nil ? "rx_repeat" : s.complete ? "rx_complete" : "rx_incomplete"
        log.log(type, [
            "id": e.id, "band": e.band.logName, "rate": e.rate.logName,
            "repeat_count": e.repeatCount,
            "kind": content?.logKind, "content_bytes": content?.contentBytes, "compressed": content == nil ? nil : compressed,
            "wire_bytes": s.totalLen, "crc32": String(format: "%08x", s.crc32),
            "frames": s.frames, "blocks_ok": s.blocksOK, "blocks": s.blocksTotal,
            "shards_have": s.shardsHave, "shards_needed": s.shardsNeeded, "repaired": s.repaired,
            "snr_db": s.snrDB, "drift_ppm": s.driftPPM, "air_s": s.airTime, "goodput_bps": s.goodputBPS,
            "f_lo": s.fLo, "f_hi": s.fHi, "carrier_snr_db": s.carrierSNR,
        ])
    }

    private func handle(_ events: [TransferEvent]) {
        for e in events {
            if e.repeatCount != nil { logEvent(e, content: nil, compressed: false) }
            if let count = e.repeatCount {
                if let i = received.firstIndex(where: { $0.transferID == e.id }) {
                    received[i].timesHeard = count
                    received[i].lastHeard = Date()
                    if e.stats.snrDB > (received[i].stats?.snrDB ?? -100) { received[i].stats = e.stats }
                    UISelectionFeedbackGenerator().selectionChanged()
                }
                continue
            }
            let (content, compressed) = e.data.map(ContentCodec.unpack) ?? (.raw(Data()), false)
            logEvent(e, content: e.data == nil ? nil : content, compressed: compressed)
            var t = Transfer(direction: .received, content: content, band: e.band, rate: e.rate,
                             transferID: e.id, payloadBytes: e.stats.totalLen,
                             airTime: e.stats.airTime, speedBPS: e.stats.goodputBPS, stats: e.stats)
            if e.data != nil {
                t.pack = PackInfo(originalLen: content.contentBytes, packedLen: e.stats.totalLen, compressed: compressed)
                // Effective speed counts the *content* delivered (compression makes it faster).
                if e.stats.airTime > 0 { t.speedBPS = Double(content.contentBytes * 8) / e.stats.airTime }
            }
            withAnimation(.spring(duration: 0.4)) {
                // A repair completes (or updates) an earlier incomplete entry.
                if let i = received.firstIndex(where: { $0.transferID == e.id && $0.incomplete }) {
                    received.remove(at: i)
                }
                received.insert(t, at: 0)
            }
            justReceived = t.id
            UINotificationFeedbackGenerator().notificationOccurred(e.stats.complete ? .success : .warning)
        }
    }

    // MARK: send

    /// Pack (framework: envelope + Brotli / WebP) and send.
    func send(_ content: TransferContent, band: BandProfile, rate: RateProfile, redundancy: Float) {
        guard !playing else { return }
        Task {
            let (payload, info, samples) = await Task.detached(priority: .userInitiated) {
                let (payload, info) = content.packed()
                let id = UInt32.random(in: 1...UInt32.max)
                let samples = payload.count <= Modem.maxPayload
                    ? Modem.encode(payload, band: band, rate: rate, id: id, redundancy: redundancy) : []
                return (payload, (info, id), samples)
            }.value
            guard !samples.isEmpty else {
                lastError = "Too large to send (\(Format.bytes(payload.count)))."
                return
            }
            var t = Transfer(direction: .sent, content: content, band: band, rate: rate, transferID: info.1,
                             payloadBytes: payload.count, airTime: 0, speedBPS: 0,
                             payload: payload, redundancySent: redundancy)
            t.pack = info.0
            log.log("tx_start", [
                "id": t.transferID, "band": band.logName, "rate": rate.logName, "redundancy": redundancy,
                "kind": content.logKind, "content_bytes": info.0.originalLen, "wire_bytes": payload.count,
                "compressed": info.0.compressed,
                "air_s": Double(samples.count) / Modem.sampleRate,
            ])
            play(samples, for: t, repair: false)
        }
    }

    /// Transmit additional, never-sent repair data for an earlier transfer.
    func sendRepair(_ transferID: UInt32) {
        guard !playing, let i = sent.firstIndex(where: { $0.transferID == transferID }),
              let payload = sent[i].payload else { return }
        let t = sent[i]
        let samples = Modem.repair(payload, band: t.band, rate: t.rate, id: t.transferID,
                                   sent: t.redundancySent, extra: Self.repairStep)
        log.log("tx_repair_start", [
            "id": t.transferID, "band": t.band.logName, "rate": t.rate.logName,
            "already_sent": t.redundancySent, "extra": Self.repairStep,
            "air_s": Double(samples.count) / Modem.sampleRate,
        ])
        sent[i].redundancySent += Self.repairStep
        play(samples, for: sent[i], repair: true)
    }

    private func play(_ samples: [Float], for t: Transfer, repair: Bool) {
        Task {
            guard await ensureAudio() else { return }
            let duration = Double(samples.count) / Modem.sampleRate
            let start = Date()
            sending = (t, start, duration, repair)
            updateRxPause()
            UIImpactFeedbackGenerator(style: .medium).impactOccurred()
            audio.play(samples) {
                Task { @MainActor in self.finishSend(t, duration: duration, repair: repair, start: start) }
            }
        }
    }

    func cancelSend() {
        audio.stopPlayback()
        if let s = sending { finishSend(s.transfer, duration: s.duration, repair: s.repair, start: s.start, cancelled: true) }
    }

    private func finishSend(_ t: Transfer, duration: Double, repair: Bool, start: Date, cancelled: Bool = false) {
        guard sending?.start == start else { return }
        sending = nil
        log.log(repair ? "tx_repair_done" : "tx_done", [
            "id": t.transferID, "cancelled": cancelled, "air_s": duration,
            "wall_s": Date().timeIntervalSince(start),
        ])
        if !cancelled {
            withAnimation {
                if repair, let i = sent.firstIndex(where: { $0.transferID == t.transferID }) {
                    sent[i].airTime += duration
                    sent[i].speedBPS = Double((sent[i].pack?.originalLen ?? sent[i].payloadBytes) * 8) / sent[i].airTime
                } else if !repair {
                    var done = t
                    done.airTime = duration
                    done.speedBPS = Double((t.pack?.originalLen ?? t.payloadBytes) * 8) / duration
                    done.wallTime = Date().timeIntervalSince(start)
                    sent.insert(done, at: 0)
                }
            }
        }
        updateRxPause()
        stopAudioIfIdle()
    }

    // MARK: lab

    func playProbe() {
        guard !playing else { return }
        Task {
            guard await ensureAudio() else { return }
            let start = Date()
            probePlaying = start
            log.log("probe_start")
            updateRxPause()
            audio.play(Modem.probe()) {
                Task { @MainActor in
                    guard self.probePlaying == start else { return }
                    self.log.log("probe_done")
                    self.probePlaying = nil
                    self.updateRxPause()
                    self.stopAudioIfIdle()
                }
            }
        }
    }

    func stopProbe() {
        log.log("probe_stopped")
        audio.stopPlayback()
        probePlaying = nil
        updateRxPause()
        stopAudioIfIdle()
    }

    func toggleRawRecording() {
        Task {
            if rawRecording {
                rawRecording = false
                rawRecordingStart = nil
                rx.stopRaw { samples in
                    let data = WAV.data(samples)
                    let stamp = Date().formatted(.iso8601.year().month().day().time(includingFractionalSeconds: false))
                        .replacingOccurrences(of: ":", with: "-")
                    let url = FileManager.default.temporaryDirectory.appendingPathComponent("recording-\(stamp).wav")
                    try? data.write(to: url)
                    EventLog.shared.log("raw_recording", ["file": url.lastPathComponent,
                                                          "seconds": Double(samples.count) / Modem.sampleRate])
                    Task { @MainActor in
                        self.rawFile = url
                        self.stopAudioIfIdle()
                    }
                }
            } else if await ensureAudio() {
                rawFile = nil
                rx.startRaw()
                log.log("raw_recording_start")
                rawRecording = true
                rawRecordingStart = Date()
            }
        }
    }
}
