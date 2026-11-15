import AVFoundation
import CoreTransferable
import Foundation
import os
import UIKit
import UniformTypeIdentifiers

/// Field log for real-world testing: JSON Lines (one object per line) in
/// Application Support/Logs, one file per day, kept for 14 days.
///
/// Every line has `ts` (ISO 8601 with ms), `t` (seconds since app start), `session`
/// (random id per app launch) and `type`. App events come from Swift; frame-level
/// receiver diagnostics (`rx_frame`, `rx_rejected`) come from the Rust framework.
/// Message contents are never logged, only sizes, kinds and checksums.
final class EventLog: @unchecked Sendable {
    static let shared = EventLog()

    let session = String(UUID().uuidString.prefix(8)).lowercased()
    let directory: URL

    private let queue = DispatchQueue(label: "tonesend.log", qos: .utility)
    private let console = Logger(subsystem: "cz.ittailors.soundtransfer", category: "log")
    private let start = Date()
    private var handle: FileHandle?
    private var handleDay = ""
    private static let keepDays = 14
    private static let maxBytes = 50 * 1024 * 1024

    private let stamp: ISO8601DateFormatter = {
        let f = ISO8601DateFormatter()
        f.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        return f
    }()

    private let dayFormat: DateFormatter = {
        let f = DateFormatter()
        f.locale = Locale(identifier: "en_US_POSIX")
        f.dateFormat = "yyyy-MM-dd"
        return f
    }()

    private init() {
        let base = FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask)[0]
        directory = base.appendingPathComponent("Logs", isDirectory: true)
        try? FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        queue.async { self.prune() }
    }

    // MARK: writing

    /// Log an event. Safe from any thread; values must be JSON-like
    /// (String, numbers, Bool, arrays, dictionaries, nil).
    func log(_ type: String, _ fields: [String: Any?] = [:]) {
        let now = Date()
        queue.async {
            var obj: [String: Any] = [:]
            for (k, v) in fields { obj[k] = Self.clean(v) }
            obj["type"] = type
            self.write(obj, at: now)
        }
    }

    /// Lines of JSON objects from the framework (`ts_receiver_take_log`).
    func logFramework(_ jsonLines: String) {
        let now = Date()
        queue.async {
            // Keep the framework's own number formatting: splice our fields in front.
            let head = "{\"session\":\"\(self.session)\",\"t\":\(Self.number(now.timeIntervalSince(self.start))),\"ts\":\"\(self.stamp.string(from: now))\","
            for line in jsonLines.split(separator: "\n") where line.hasPrefix("{\"") {
                let full = head + line.dropFirst()
                self.console.debug("\(full, privacy: .public)")
                self.fileHandle(for: now)?.write(Data((full + "\n").utf8))
            }
        }
    }

    private func write(_ body: [String: Any], at date: Date) {
        var obj = body
        obj["ts"] = stamp.string(from: date)
        obj["t"] = Self.number(date.timeIntervalSince(start))
        obj["session"] = session
        guard JSONSerialization.isValidJSONObject(obj),
              var data = try? JSONSerialization.data(withJSONObject: obj, options: [.sortedKeys, .withoutEscapingSlashes])
        else { return }
        if let line = String(data: data, encoding: .utf8) {
            console.debug("\(line, privacy: .public)")
        }
        data.append(0x0A)
        fileHandle(for: date)?.write(data)
    }

    private func fileHandle(for date: Date) -> FileHandle? {
        let day = dayFormat.string(from: date)
        if day == handleDay, let handle { return handle }
        try? handle?.close()
        let url = directory.appendingPathComponent("tonesend-\(day).jsonl")
        if !FileManager.default.fileExists(atPath: url.path) {
            FileManager.default.createFile(atPath: url.path, contents: nil)
        }
        handle = try? FileHandle(forWritingTo: url)
        _ = try? handle?.seekToEnd()
        handleDay = day
        return handle
    }

    /// Decimal with at most 3 fractional digits (a Double would print as 0.045999999999999999).
    private static func number(_ x: Double) -> Any {
        x.isFinite ? NSDecimalNumber(string: String(format: "%.3f", x), locale: Locale(identifier: "en_US_POSIX")) : NSNull()
    }

    /// NaN/∞ are not valid JSON; Float/CGFloat become Double, nested values are cleaned.
    private static func clean(_ v: Any?) -> Any {
        switch v {
        case nil: return NSNull()
        case let x as Double: return number(x)
        case let x as Float: return clean(Double(x))
        case let x as CGFloat: return clean(Double(x))
        case let x as [Float]: return x.map { clean(Double($0)) }
        case let x as [Any?]: return x.map { clean($0) }
        case let x as [String: Any?]: return x.mapValues { clean($0) }
        case let x as String: return x
        case let x as Bool: return x
        case let x as any BinaryInteger: return Int(truncatingIfNeeded: x)
        case let x?: return String(describing: x)
        }
    }

    // MARK: files

    private var files: [URL] {
        ((try? FileManager.default.contentsOfDirectory(at: directory, includingPropertiesForKeys: [.fileSizeKey])) ?? [])
            .filter { $0.pathExtension == "jsonl" }
            .sorted { $0.lastPathComponent < $1.lastPathComponent }
    }

    private func size(_ url: URL) -> Int {
        (try? url.resourceValues(forKeys: [.fileSizeKey]).fileSize) ?? 0
    }

    /// Drop files older than `keepDays`, then the oldest until under `maxBytes`.
    private func prune() {
        let cutoff = dayFormat.string(from: Date().addingTimeInterval(-Double(Self.keepDays) * 86400))
        var list = files
        for f in list where f.lastPathComponent < "tonesend-\(cutoff).jsonl" {
            try? FileManager.default.removeItem(at: f)
        }
        list = files
        var total = list.reduce(0) { $0 + size($1) }
        for f in list.dropLast() where total > Self.maxBytes {
            total -= size(f)
            try? FileManager.default.removeItem(at: f)
        }
    }

    /// Total size of all log files (bytes).
    func totalSize() -> Int {
        queue.sync { files.reduce(0) { $0 + size($1) } }
    }

    /// All log files concatenated into one file for sharing.
    func export() -> URL? {
        queue.sync {
            try? handle?.synchronize()
            let machine = DeviceInfo.machine
            let f = DateFormatter()
            f.locale = Locale(identifier: "en_US_POSIX")
            f.dateFormat = "yyyyMMdd-HHmm"
            let url = FileManager.default.temporaryDirectory
                .appendingPathComponent("tonesend-log-\(machine)-\(f.string(from: Date())).jsonl")
            var out = Data()
            for file in files { out.append((try? Data(contentsOf: file)) ?? Data()) }
            do { try out.write(to: url) } catch { return nil }
            return url
        }
    }

    func clear() {
        queue.sync {
            try? handle?.close()
            handle = nil
            handleDay = ""
            for f in files { try? FileManager.default.removeItem(at: f) }
        }
        log("log_cleared")
    }
}

/// Shareable log file (exported when the share sheet asks for it).
struct LogExport: Transferable {
    static var transferRepresentation: some TransferRepresentation {
        FileRepresentation(exportedContentType: .plainText) { _ in
            guard let url = EventLog.shared.export() else { throw CocoaError(.fileWriteUnknown) }
            return SentTransferredFile(url)
        }
    }
}

// MARK: device & audio context

enum DeviceInfo {
    static var machine: String {
        if let sim = ProcessInfo.processInfo.environment["SIMULATOR_MODEL_IDENTIFIER"] { return "\(sim) (simulator)" }
        var u = utsname()
        uname(&u)
        return withUnsafeBytes(of: &u.machine) { String(decoding: $0.prefix { $0 != 0 }, as: UTF8.self) }
    }

    /// Random id kept for the life of the installation (tells test phones apart in logs).
    static var installID: String {
        let key = "log.installID"
        if let id = UserDefaults.standard.string(forKey: key) { return id }
        let id = String(UUID().uuidString.prefix(6)).lowercased()
        UserDefaults.standard.set(id, forKey: key)
        return id
    }

    @MainActor static var fields: [String: Any?] {
        let info = Bundle.main.infoDictionary ?? [:]
        let thermal: String = switch ProcessInfo.processInfo.thermalState {
        case .nominal: "nominal"
        case .fair: "fair"
        case .serious: "serious"
        case .critical: "critical"
        @unknown default: "unknown"
        }
        return [
            "install_id": installID,
            "app_version": info["CFBundleShortVersionString"] as? String,
            "build": info["CFBundleVersion"] as? String,
            "model": machine,
            "system": "\(UIDevice.current.systemName) \(UIDevice.current.systemVersion)",
            "locale": Locale.current.identifier,
            "low_power": ProcessInfo.processInfo.isLowPowerModeEnabled,
            "thermal": thermal,
        ]
    }

    /// Current audio route and hardware settings.
    static var audioFields: [String: Any?] {
        let s = AVAudioSession.sharedInstance()
        func ports(_ list: [AVAudioSessionPortDescription]) -> [Any?] {
            list.map { p in
                ["type": p.portType.rawValue, "name": p.portName,
                 "data_source": p.selectedDataSource?.dataSourceName,
                 "polar": p.selectedDataSource?.selectedPolarPattern?.rawValue] as [String: Any?]
            }
        }
        return [
            "inputs": ports(s.currentRoute.inputs),
            "outputs": ports(s.currentRoute.outputs),
            "category": s.category.rawValue,
            "mode": s.mode.rawValue,
            "sample_rate": s.sampleRate,
            "io_buffer_s": s.ioBufferDuration,
            "input_latency_s": s.inputLatency,
            "output_latency_s": s.outputLatency,
            "input_channels": s.inputNumberOfChannels,
            "output_volume": s.outputVolume,
            "input_gain": s.isInputGainSettable ? Double(s.inputGain) : nil,
        ]
    }
}

// MARK: names used in the log (same as the framework / CLI)

extension BandProfile {
    var logName: String {
        switch self {
        case .wide: "wide"
        case .narrow: "narrow"
        case .inaudible: "inaudible"
        case .max: "max"
        case .high: "high"
        }
    }
}

extension RateProfile {
    var logName: String {
        switch self {
        case .safe: "safe"
        case .normal: "normal"
        case .fast: "fast"
        case .turbo: "turbo"
        }
    }
}

extension TransferContent {
    var logKind: String {
        switch self {
        case .text: "text"
        case .file: "file"
        case .image: "image"
        case .raw: "raw"
        }
    }
}
