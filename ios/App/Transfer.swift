import Foundation
import ToneSendKit
import UIKit

/// What a transfer carried. Packing (envelope, Brotli compression, WebP images)
/// is done by the Rust framework (`content` module) — identical on every platform.
enum TransferContent: Hashable {
    case text(String)
    case file(name: String, data: Data)
    /// Image file produced by the framework (WebP), or a received JPEG/PNG.
    case image(name: String, data: Data)
    /// Payload without a recognised envelope (or damaged).
    case raw(Data)

    /// Bytes to send (envelope + possibly compressed body) and what packing did.
    func packed() -> (Data, PackInfo) {
        switch self {
        case .text(let s): ContentCodec.packText(s)
        case .file(let name, let data): ContentCodec.packFile(name: name, data: data)
        case .image(let name, let data): ContentCodec.packImage(name: name, data: data)
        case .raw(let data): (data, PackInfo(originalLen: data.count, packedLen: data.count, compressed: false))
        }
    }

    var image: UIImage? {
        switch self {
        case .image(_, let data): ContentCodec.image(from: data)
        case .file(_, let data): UIImage(data: data)
        default: nil
        }
    }

    var title: String {
        switch self {
        case .text(let s): s.isEmpty ? "(empty message)" : s
        case .file(let name, _): name
        case .image(let name, _): name
        case .raw: "Unrecognised data"
        }
    }

    var kindSymbol: String {
        switch self {
        case .text: "text.bubble.fill"
        case .image: "photo.fill"
        case .file: image != nil ? "photo.fill" : "doc.fill"
        case .raw: "questionmark.square.dashed"
        }
    }

    /// Size of the content itself (before compression).
    var contentBytes: Int {
        switch self {
        case .text(let s): s.utf8.count
        case .file(_, let data), .image(_, let data), .raw(let data): data.count
        }
    }
}

struct PackInfo: Hashable {
    var originalLen: Int
    var packedLen: Int
    var compressed: Bool

    /// Fraction saved by packing (0.57 = 57 % smaller).
    var saving: Double { originalLen > 0 ? max(0, 1 - Double(packedLen) / Double(originalLen)) : 0 }
}

/// Swift face of the framework's content layer.
enum ContentCodec {
    private static func take(_ p: UnsafeMutablePointer<UInt8>?, _ n: Int) -> Data {
        guard let p else { return Data() }
        defer { ts_bytes_free(p, n) }
        return Data(bytes: p, count: n)
    }

    private static func info(_ i: TsPackInfo) -> PackInfo {
        PackInfo(originalLen: Int(i.original_len), packedLen: Int(i.packed_len), compressed: i.compressed != 0)
    }

    static func packText(_ s: String) -> (Data, PackInfo) {
        var i = TsPackInfo(), n = 0
        let utf8 = Array(s.utf8)
        let p = utf8.withUnsafeBufferPointer { b in
            b.baseAddress!.withMemoryRebound(to: CChar.self, capacity: max(b.count, 1)) {
                ts_pack_text($0, b.count, &i, &n)
            }
        }
        return (take(p, n), info(i))
    }

    private static func pack(_ name: String, _ data: Data,
                             _ f: (UnsafePointer<CChar>?, Int, UnsafePointer<UInt8>?, Int, UnsafeMutablePointer<TsPackInfo>?, UnsafeMutablePointer<Int>?) -> UnsafeMutablePointer<UInt8>?) -> (Data, PackInfo) {
        var i = TsPackInfo(), n = 0
        let nm = Array(name.utf8CString)
        let p = nm.withUnsafeBufferPointer { nb in
            data.withUnsafeBytes { db in
                f(nb.baseAddress, nm.count - 1, db.bindMemory(to: UInt8.self).baseAddress, data.count, &i, &n)
            }
        }
        return (take(p, n), info(i))
    }

    static func packFile(name: String, data: Data) -> (Data, PackInfo) { pack(name, data, ts_pack_file) }
    static func packImage(name: String, data: Data) -> (Data, PackInfo) { pack(name, data, ts_pack_image) }

    /// Downscale + WebP in the framework. Returns the image file and its pixel size.
    static func encodeImage(_ img: UIImage, maxSide: Int, quality: Float) -> (Data, Int, Int)? {
        // Render to RGBA8 (bounded to keep memory small; the framework does the final resize).
        let limit = CGFloat(max(maxSide * 4, 1024))
        let scale = min(1, limit / max(img.size.width, img.size.height))
        let w = max(1, Int(img.size.width * scale)), h = max(1, Int(img.size.height * scale))
        var px = [UInt8](repeating: 0, count: w * h * 4)
        let ok: Bool = px.withUnsafeMutableBytes { buf in
            guard let ctx = CGContext(data: buf.baseAddress, width: w, height: h, bitsPerComponent: 8, bytesPerRow: w * 4,
                                      space: CGColorSpaceCreateDeviceRGB(),
                                      bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue) else { return false }
            UIGraphicsPushContext(ctx)
            ctx.translateBy(x: 0, y: CGFloat(h))
            ctx.scaleBy(x: 1, y: -1)
            img.draw(in: CGRect(x: 0, y: 0, width: w, height: h))
            UIGraphicsPopContext()
            return true
        }
        guard ok else { return nil }
        var ow: UInt32 = 0, oh: UInt32 = 0, n = 0
        let p = ts_image_encode_rgba(px, UInt32(w), UInt32(h), UInt32(maxSide), quality, &ow, &oh, &n)
        guard p != nil else { return nil }
        return (take(p, n), Int(ow), Int(oh))
    }

    private static let cache = NSCache<NSData, UIImage>()

    /// WebP (via the framework) or any UIKit-readable image.
    static func image(from data: Data) -> UIImage? {
        if let hit = cache.object(forKey: data as NSData) { return hit }
        var w: UInt32 = 0, h: UInt32 = 0, n = 0
        let p = data.withUnsafeBytes { ts_image_decode($0.bindMemory(to: UInt8.self).baseAddress, data.count, &w, &h, &n) }
        var result: UIImage?
        if let p {
            let px = Data(bytes: p, count: n)
            ts_bytes_free(p, n)
            if let provider = CGDataProvider(data: px as CFData),
               let cg = CGImage(width: Int(w), height: Int(h), bitsPerComponent: 8, bitsPerPixel: 32, bytesPerRow: Int(w) * 4,
                                space: CGColorSpaceCreateDeviceRGB(), bitmapInfo: CGBitmapInfo(rawValue: CGImageAlphaInfo.last.rawValue),
                                provider: provider, decode: nil, shouldInterpolate: true, intent: .defaultIntent) {
                result = UIImage(cgImage: cg)
            }
        } else {
            result = UIImage(data: data)
        }
        if let result { cache.setObject(result, forKey: data as NSData) }
        return result
    }

    /// Unpack received bytes. Returns the content and whether it was compressed on air.
    static func unpack(_ data: Data) -> (TransferContent, Bool) {
        guard let c = data.withUnsafeBytes({ ts_unpack($0.bindMemory(to: UInt8.self).baseAddress, data.count) }) else {
            return (.raw(data), false)
        }
        defer { ts_content_free(c) }
        let v = c.pointee
        let body = v.data_len > 0 ? Data(bytes: v.data, count: v.data_len) : Data()
        let name = v.name_len > 0
            ? String(decoding: UnsafeBufferPointer(start: UnsafeRawPointer(v.name).assumingMemoryBound(to: UInt8.self), count: v.name_len), as: UTF8.self)
            : ""
        let content: TransferContent = switch v.kind {
        case 0: .text(String(decoding: body, as: UTF8.self))
        case 1: .file(name: name, data: body)
        case 2: .image(name: name.isEmpty ? "photo.webp" : name, data: body)
        default: .raw(body)
        }
        return (content, v.compressed != 0)
    }
}

enum Direction: String { case sent, received }

struct Transfer: Identifiable, Hashable {
    let id = UUID()
    var date = Date()
    var direction: Direction
    var content: TransferContent
    var band: BandProfile
    var rate: RateProfile
    /// Transfer-layer id shared by sender and receiver (matches repairs to the original).
    var transferID: UInt32
    /// Bytes on air (envelope + compressed body).
    var payloadBytes: Int
    /// Packing result: content size before compression, and whether Brotli was used.
    var pack: PackInfo?
    /// Air time in seconds (all frames, repair data included).
    var airTime: Double
    /// File bits per second of air time.
    var speedBPS: Double
    /// Receiver-side statistics.
    var stats: LinkStats?
    /// Sender: wall-clock duration, encoded payload (for repairs), repair data sent so far.
    var wallTime: Double?
    var payload: Data?
    var redundancySent: Float = 0
    /// Looped messages (IDs in music/ads): how often and when last heard.
    var timesHeard = 1
    var lastHeard: Date?

    var ok: Bool { stats?.ok ?? true }
    var incomplete: Bool { direction == .received && !(stats?.complete ?? true) }

    var title: String {
        if incomplete, let s = stats {
            return "Incomplete — \(s.missing) of \(s.shardsNeeded) blocks missing"
        }
        return content.title
    }

    /// File URL in the temp dir for sharing (files, images, long text).
    func shareURL() -> URL? {
        let dir = FileManager.default.temporaryDirectory.appendingPathComponent(id.uuidString, isDirectory: true)
        try? FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        let (name, data): (String, Data) = switch content {
        case .text(let s): ("message.txt", Data(s.utf8))
        case .file(let n, let d), .image(let n, let d): (n.isEmpty ? "file" : n, d)
        case .raw(let d): ("payload.bin", d)
        }
        let url = dir.appendingPathComponent(name)
        do { try data.write(to: url) } catch { return nil }
        return url
    }
}

enum Format {
    static func bytes(_ n: Int) -> String {
        ByteCountFormatter.string(fromByteCount: Int64(n), countStyle: .file)
    }

    static func speed(_ bps: Double) -> String {
        bps >= 1000 ? String(format: "%.2f kbit/s", bps / 1000) : String(format: "%.0f bit/s", bps)
    }

    static func seconds(_ s: Double) -> String {
        s < 60 ? String(format: "%.1f s", s) : String(format: "%d:%02d", Int(s) / 60, Int(s) % 60)
    }
}
