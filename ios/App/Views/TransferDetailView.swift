import Charts
import SwiftUI

struct TransferDetailView: View {
    @EnvironmentObject var model: AppModel
    var transfer: Transfer
    @State private var copied = false

    /// Live version (repairs update sent/received entries after this view opened).
    private var t: Transfer {
        let list = transfer.direction == .sent ? model.sent : model.received
        return list.first { $0.transferID == transfer.transferID } ?? transfer
    }

    var body: some View {
        ScrollView {
            VStack(spacing: 16) {
                header
                if t.timesHeard > 1, let last = t.lastHeard {
                    Label {
                        Text("Heard \(t.timesHeard)× · last at \(last.formatted(date: .omitted, time: .standard))")
                    } icon: {
                        Image(systemName: "repeat")
                    }
                    .font(.subheadline)
                    .foregroundStyle(.secondary)
                    .frame(maxWidth: .infinity, alignment: .leading)
                }
                if t.incomplete { incompleteCard } else { contentCard }
                if t.direction == .sent { repairCard }
                statsGrid
                if let s = t.stats, !s.carrierSNR.isEmpty { spectrumCard(s) }
            }
            .padding()
        }
        .background(Color(.systemGroupedBackground))
        .navigationTitle(t.direction == .sent ? "Sent" : "Received")
        .navigationBarTitleDisplayMode(.inline)
        .toolbar {
            if !t.incomplete, let url = t.shareURL() {
                ShareLink(item: url)
            }
        }
    }

    private var header: some View {
        HStack {
            ProfileTag(band: t.band, rate: t.rate)
            Spacer()
            if t.direction == .received {
                Label(t.incomplete ? "Incomplete" : "Checksum OK",
                      systemImage: t.incomplete ? "exclamationmark.triangle.fill" : "checkmark.seal.fill")
                    .font(.caption.weight(.semibold))
                    .foregroundStyle(t.incomplete ? .orange : .green)
            }
            Text(t.date, format: .dateTime.hour().minute().second())
                .font(.caption).foregroundStyle(.secondary)
        }
    }

    private var incompleteCard: some View {
        Card {
            VStack(alignment: .leading, spacing: 10) {
                Label("Not enough blocks to rebuild the file yet", systemImage: "bandage.fill")
                    .font(.headline).foregroundStyle(.orange)
                if let s = t.stats {
                    ProgressView(value: Double(s.shardsHave), total: Double(max(s.shardsNeeded, 1))).tint(.orange)
                    Text("\(s.shardsHave) of \(s.shardsNeeded) blocks · \(s.missing) missing · \(Format.bytes(s.totalLen))")
                        .font(.caption.monospacedDigit()).foregroundStyle(.secondary)
                }
                Text("Keep listening and tap **Send repair data** on the sending phone. New repair blocks are merged with what was already received.")
                    .font(.subheadline)
            }
        }
    }

    @ViewBuilder private var contentCard: some View {
        Card {
            switch t.content {
            case .text(let s):
                VStack(alignment: .leading, spacing: 12) {
                    Text(s).font(.body).textSelection(.enabled)
                    Button {
                        UIPasteboard.general.string = s
                        copied = true
                    } label: {
                        Label(copied ? "Copied" : "Copy", systemImage: copied ? "checkmark" : "doc.on.doc")
                    }
                    .buttonStyle(.bordered)
                }
            case .file(let name, let data), .image(let name, let data):
                VStack(alignment: .leading, spacing: 12) {
                    if let img = t.content.image {
                        Image(uiImage: img).resizable().scaledToFit()
                            .frame(maxWidth: .infinity)
                            .clipShape(RoundedRectangle(cornerRadius: 12))
                        Text("\(Int(img.size.width))×\(Int(img.size.height)) · \(Format.bytes(data.count))")
                            .font(.caption).foregroundStyle(.secondary)
                    } else {
                        Label(name, systemImage: "doc.fill").font(.headline)
                        Text(Format.bytes(data.count)).font(.caption).foregroundStyle(.secondary)
                    }
                    if let url = t.shareURL() {
                        ShareLink(item: url) { Label("Share / Save", systemImage: "square.and.arrow.up") }
                            .buttonStyle(.bordered)
                    }
                }
            case .raw(let data):
                VStack(alignment: .leading, spacing: 8) {
                    Label("Unrecognised data", systemImage: "questionmark.square.dashed").font(.headline)
                    Text(data.prefix(64).map { String(format: "%02x", $0) }.joined(separator: " "))
                        .font(.caption.monospaced()).foregroundStyle(.secondary)
                }
            }
        }
    }

    private var repairCard: some View {
        Card {
            VStack(alignment: .leading, spacing: 10) {
                SectionTitle(text: "Repair", trailing: "\(Int((t.redundancySent * 100).rounded())) % sent")
                Text("If the receiver shows the transfer as incomplete, send extra repair blocks. It merges them with what it already has — no need to resend the file.")
                    .font(.caption).foregroundStyle(.secondary)
                Button {
                    model.sendRepair(t.transferID)
                } label: {
                    Label("Send repair data (+\(Int(AppModel.repairStep * 100)) %)", systemImage: "cross.case.fill")
                        .frame(maxWidth: .infinity)
                }
                .buttonStyle(.borderedProminent)
                .disabled(model.sending != nil || t.payload == nil)
                if let s = model.sending, s.repair, s.transfer.transferID == t.transferID {
                    TimelineView(.periodic(from: s.start, by: 0.1)) { ctx in
                        ProgressView(value: min(1, ctx.date.timeIntervalSince(s.start) / max(s.duration, 0.01))).tint(.brand)
                    }
                }
            }
        }
    }

    private var statsGrid: some View {
        let cols = [GridItem(.flexible(), spacing: 10), GridItem(.flexible(), spacing: 10)]
        return LazyVGrid(columns: cols, spacing: 10) {
            if !t.incomplete {
                StatTile(title: t.direction == .received ? "Effective speed" : "Speed",
                         value: Format.speed(t.speedBPS), symbol: "speedometer")
            }
            StatTile(title: "Link rate", value: Format.speed(Modem.bitrate(t.band, t.rate)), symbol: "bolt.horizontal")
            StatTile(title: "Air time", value: Format.seconds(t.airTime), symbol: "timer")
            StatTile(title: "Content", value: Format.bytes(t.content.contentBytes), symbol: "doc")
            StatTile(title: "On air", value: Format.bytes(t.payloadBytes), symbol: "shippingbox")
            StatTile(title: "Compression", value: compressionText, symbol: "arrow.down.right.and.arrow.up.left",
                     tint: (t.pack?.saving ?? 0) > 0.05 ? .green : .brand)
            if let s = t.stats {
                StatTile(title: "Checksum (CRC-32)", value: String(format: "%08X", s.crc32), symbol: "number",
                         tint: s.complete ? .green : .orange)
                StatTile(title: "Repaired blocks", value: "\(s.repaired)", symbol: "bandage",
                         tint: s.repaired > 0 ? .green : .brand)
                StatTile(title: "Blocks received", value: "\(s.blocksOK)/\(s.blocksTotal)", symbol: "square.grid.3x3.fill",
                         tint: s.blocksOK == s.blocksTotal ? .green : .orange)
                StatTile(title: "Packets", value: "\(s.frames)", symbol: "square.stack.3d.up")
                StatTile(title: "Signal (SNR)", value: String(format: "%.1f dB", s.snrDB), symbol: "waveform.path.ecg",
                         tint: s.snrDB >= 12 ? .green : s.snrDB >= 8 ? .yellow : .orange)
                StatTile(title: "Clock drift", value: String(format: "%+.0f ppm", s.driftPPM.rounded() + 0), symbol: "clock.arrow.2.circlepath")
            } else {
                StatTile(title: "Repair data", value: "\(Int((t.redundancySent * 100).rounded())) %", symbol: "cross.case")
                if let wall = t.wallTime {
                    StatTile(title: "Wall time", value: Format.seconds(wall), symbol: "stopwatch")
                }
            }
        }
    }

    private var compressionText: String {
        if case .image = t.content { return "WebP image" }
        guard let p = t.pack, !t.incomplete else { return "—" }
        return p.compressed ? "−\(Int((p.saving * 100).rounded())) % Brotli" : "none (not compressible)"
    }

    private func spectrumCard(_ s: LinkStats) -> some View {
        let n = s.carrierSNR.count
        let step = n > 1 ? (s.fHi - s.fLo) / Float(n - 1) : 0
        let points = s.carrierSNR.enumerated().map { (i, v) in (khz: Double(s.fLo + Float(i) * step) / 1000, snr: Double(v)) }
        let need = t.rate.requiredSNR
        return Card {
            VStack(alignment: .leading, spacing: 8) {
                SectionTitle(text: "Signal per frequency", trailing: "last packet · \(n) carriers")
                Chart {
                    ForEach(points, id: \.khz) { p in
                        AreaMark(x: .value("kHz", p.khz), y: .value("SNR", p.snr))
                            .foregroundStyle(LinearGradient(colors: [.brand.opacity(0.5), .brand.opacity(0.05)],
                                                            startPoint: .top, endPoint: .bottom))
                        LineMark(x: .value("kHz", p.khz), y: .value("SNR", p.snr))
                            .foregroundStyle(Color.brand)
                    }
                    RuleMark(y: .value("Needed", need))
                        .foregroundStyle(.orange)
                        .lineStyle(StrokeStyle(lineWidth: 1, dash: [4, 3]))
                        .annotation(position: .top, alignment: .leading) {
                            Text("needed for \(t.rate.title)").font(.caption2).foregroundStyle(.orange)
                        }
                }
                .chartXScale(domain: Double(s.fLo) / 1000 ... Double(s.fHi) / 1000)
                .chartXAxisLabel("kHz")
                .chartYAxisLabel("dB")
                .frame(height: 180)
            }
        }
    }
}
