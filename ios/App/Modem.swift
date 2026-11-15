import Foundation
import ToneSendKit

/// Frequency band profile (must match the Rust `Band` order).
enum BandProfile: Int, CaseIterable, Identifiable, Codable {
    case max = 3, wide = 0, narrow = 1, inaudible = 2, high = 4

    var id: Int { rawValue }
    var c: UInt8 { UInt8(rawValue) }

    var title: String {
        switch self {
        case .max: "Max"
        case .wide: "Audible"
        case .narrow: "Robust"
        case .inaudible: "Silent"
        case .high: "High"
        }
    }

    var detail: String {
        switch self {
        case .max: "Audible · 1–13 kHz · close range"
        case .wide: "Audible · 1–8 kHz"
        case .narrow: "Audible · 1.5–5 kHz · long symbols"
        case .inaudible: "Near-ultrasonic · 17.5–20.5 kHz"
        case .high: "14.6–16.4 kHz · survives MP3/AAC · for music & ads"
        }
    }

    var symbol: String {
        switch self {
        case .max: "hare.fill"
        case .wide: "bolt.fill"
        case .narrow: "shield.lefthalf.filled"
        case .inaudible: "speaker.slash.fill"
        case .high: "music.note"
        }
    }
}

/// Modulation + coding rate (must match the Rust `Rate` ids).
enum RateProfile: Int, CaseIterable, Identifiable, Codable {
    case safe = 0, normal = 1, fast = 2, turbo = 3

    var id: Int { rawValue }
    var c: UInt8 { UInt8(rawValue) }

    var title: String {
        switch self {
        case .safe: "Safe"
        case .normal: "Normal"
        case .fast: "Fast"
        case .turbo: "Turbo"
        }
    }

    var detail: String {
        switch self {
        case .safe: "DBPSK · FEC ½"
        case .normal: "DQPSK · FEC ½"
        case .fast: "DQPSK · FEC ¾"
        case .turbo: "D8PSK · FEC ¾ · needs strong signal"
        }
    }

    /// Signal-to-noise the decoder needs for this rate (dB, simulator-calibrated).
    var requiredSNR: Double {
        switch self {
        case .safe: 5
        case .normal: 8
        case .fast: 10
        case .turbo: 16
        }
    }
}

enum Modem {
    static let sampleRate = Double(TS_SAMPLE_RATE)

    /// Audio for a transfer (checksum + `redundancy` fraction of repair data).
    static func encode(_ data: Data, band: BandProfile, rate: RateProfile, id: UInt32, redundancy: Float) -> [Float] {
        var n = 0
        let ptr: UnsafeMutablePointer<Float>? = data.withUnsafeBytes { raw in
            ts_transfer_encode(raw.bindMemory(to: UInt8.self).baseAddress, data.count, band.c, rate.c, id, redundancy, &n)
        }
        return takeSamples(ptr, n)
    }

    /// Audio with additional repair data the receiver has not seen yet.
    static func repair(_ data: Data, band: BandProfile, rate: RateProfile, id: UInt32, sent: Float, extra: Float) -> [Float] {
        var n = 0
        let ptr: UnsafeMutablePointer<Float>? = data.withUnsafeBytes { raw in
            ts_transfer_repair(raw.bindMemory(to: UInt8.self).baseAddress, data.count, band.c, rate.c, id, sent, extra, &n)
        }
        return takeSamples(ptr, n)
    }

    static func probe(frameBytes: Int = 1024) -> [Float] {
        var n = 0
        return takeSamples(ts_probe_generate(frameBytes, &n), n)
    }

    /// Error-free link bitrate (bit/s) of a profile for a large payload.
    static func bitrate(_ band: BandProfile, _ rate: RateProfile, bytes: Int = 65536) -> Double {
        ts_net_bitrate(band.c, rate.c, max(bytes, 1))
    }

    /// Air time of a transfer of `bytes` with `redundancy`.
    static func airTime(_ band: BandProfile, _ rate: RateProfile, bytes: Int, redundancy: Float) -> Double {
        ts_transfer_seconds(bytes, band.c, rate.c, redundancy)
    }

    static var maxPayload: Int { ts_max_payload() }

    private static func takeSamples(_ ptr: UnsafeMutablePointer<Float>?, _ n: Int) -> [Float] {
        guard let ptr else { return [] }
        defer { ts_samples_free(ptr, n) }
        return Array(UnsafeBufferPointer(start: ptr, count: n))
    }
}

/// Receiver-side statistics of one transfer.
struct LinkStats: Hashable, Codable {
    var complete: Bool
    var crc32: UInt32
    var totalLen: Int
    var frames: Int
    var blocksOK: Int
    var blocksTotal: Int
    var shardsHave: Int
    var shardsNeeded: Int
    var repaired: Int
    var snrDB: Float
    var driftPPM: Float
    var airTime: Double
    var goodputBPS: Double
    var carrierSNR: [Float]
    var fLo: Float
    var fHi: Float

    var ok: Bool { complete }
    var missing: Int { max(0, shardsNeeded - shardsHave) }
}

struct TransferEvent {
    var id: UInt32
    var band: BandProfile
    var rate: RateProfile
    var data: Data?          // nil = incomplete or repeat
    var stats: LinkStats
    /// Set when a looped message (e.g. in a song) was heard again: times heard so far.
    var repeatCount: Int?
}

enum ReceiverActivity: Equatable {
    case listening
    case detected(BandProfile)
    case receiving(BandProfile, RateProfile, bytes: Int, progress: Float)
}

struct TransferProgress: Equatable {
    var id: UInt32
    var totalLen: Int
    var have: Int
    var needed: Int
    var lost: Int
}

struct ReceiverStatus: Equatable {
    var activity: ReceiverActivity = .listening
    var levelDB: Float = -120
    var transfer: TransferProgress?
}

/// Streaming decoder + transfer assembler. Not thread-safe: use from one queue.
final class ModemReceiver {
    private let ptr: OpaquePointer

    init(bands: Set<BandProfile>) {
        let mask = bands.reduce(UInt32(0)) { $0 | (1 << UInt32($1.rawValue)) }
        ptr = ts_receiver_new(mask)
    }

    deinit { ts_receiver_free(ptr) }

    func push(_ samples: [Float]) -> [TransferEvent] {
        let queued = samples.withUnsafeBufferPointer { ts_receiver_push(ptr, $0.baseAddress, $0.count) }
        guard queued > 0 else { return [] }
        var out: [TransferEvent] = []
        while let e = ts_receiver_next_event(ptr) {
            defer { ts_event_free(e) }
            let c = e.pointee
            let complete = c.kind == 1
            let repeatCount: Int? = c.kind == 3 ? Int(c.repeat_count) : nil
            let snr = c.carrier_count > 0 ? Array(UnsafeBufferPointer(start: c.carrier_snr_db, count: c.carrier_count)) : []
            out.append(TransferEvent(
                id: c.transfer_id,
                band: BandProfile(rawValue: Int(c.band)) ?? .wide,
                rate: RateProfile(rawValue: Int(c.rate)) ?? .normal,
                data: complete ? Data(bytes: c.data, count: c.data_len) : nil,
                stats: LinkStats(
                    complete: complete, crc32: c.crc32, totalLen: Int(c.total_len), frames: Int(c.frames),
                    blocksOK: Int(c.blocks_ok), blocksTotal: Int(c.blocks_total),
                    shardsHave: Int(c.shards_have), shardsNeeded: Int(c.shards_needed), repaired: Int(c.repaired),
                    snrDB: c.snr_db, driftPPM: c.drift_ppm, airTime: Double(c.air_time),
                    goodputBPS: Double(c.goodput_bps), carrierSNR: snr, fLo: c.f_lo, fHi: c.f_hi),
                repeatCount: repeatCount))
        }
        return out
    }

    var status: ReceiverStatus {
        let s = ts_receiver_status(ptr)
        let band = BandProfile(rawValue: Int(s.band)) ?? .wide
        let activity: ReceiverActivity = switch s.activity {
        case 1: .detected(band)
        case 2: .receiving(band, RateProfile(rawValue: Int(s.rate)) ?? .normal, bytes: Int(s.bytes), progress: s.progress)
        default: .listening
        }
        let t = s.transfer_active != 0
            ? TransferProgress(id: s.transfer_id, totalLen: Int(s.transfer_len), have: Int(s.shards_have),
                               needed: Int(s.shards_needed), lost: Int(s.blocks_lost))
            : nil
        return ReceiverStatus(activity: activity, levelDB: s.level_db, transfer: t)
    }
}
