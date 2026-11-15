import SwiftUI

struct ReceiveView: View {
    @EnvironmentObject var model: AppModel
    @State private var path: [Transfer] = []

    var body: some View {
        NavigationStack(path: $path) {
            ScrollView {
                VStack(spacing: 16) {
                    listenCard
                    if model.listening { activityCard }
                    if model.micDenied { micDeniedCard }
                    if !model.received.isEmpty { summary }
                    feed
                }
                .padding()
            }
            .background(Color(.systemGroupedBackground))
            .navigationTitle("Receive")
            .navigationDestination(for: Transfer.self) { TransferDetailView(transfer: $0) }
            .onChange(of: model.received.count) { _, _ in
                // `-openFirst YES` (screenshots/demo): open the newest transfer.
                if UserDefaults.standard.bool(forKey: "openFirst"), path.isEmpty, let t = model.received.first { path = [t] }
            }
            .onAppear {
                if UserDefaults.standard.bool(forKey: "openFirst"), path.isEmpty, let t = model.received.first { path = [t] }
            }
            .toolbar {
                if !model.received.isEmpty {
                    Button("Clear") { withAnimation { model.clearReceived() } }
                }
            }
        }
    }

    private var isReceiving: Bool {
        if case .listening = model.status.activity { return false }
        return model.listening
    }

    private var listenCard: some View {
        Card(padding: 24) {
            VStack(spacing: 18) {
                ListenOrb(active: model.listening, levelDB: model.status.levelDB, receiving: isReceiving) {
                    model.toggleListening()
                }
                .frame(maxWidth: .infinity)
                Text(model.listening ? "Listening on all enabled bands" : "Tap to start listening")
                    .font(.subheadline)
                    .foregroundStyle(.secondary)
                if model.listening {
                    HStack(spacing: 8) {
                        Image(systemName: "mic.fill").foregroundStyle(.secondary)
                        LevelBar(levelDB: model.status.levelDB)
                        Text(String(format: "%.0f dB", model.status.levelDB))
                            .font(.caption.monospacedDigit())
                            .foregroundStyle(.secondary)
                            .frame(width: 48, alignment: .trailing)
                    }
                }
            }
        }
    }

    @ViewBuilder private var activityCard: some View {
        let t = model.status.transfer
        switch model.status.activity {
        case .listening where t == nil:
            Card {
                Label("Waiting for a transmission…", systemImage: "antenna.radiowaves.left.and.right")
                    .foregroundStyle(.secondary)
                    .frame(maxWidth: .infinity, alignment: .leading)
            }
        case .detected(let band) where t == nil:
            Card {
                HStack {
                    ProgressView()
                    Text("Signal detected on \(band.title) band — reading header…")
                        .font(.subheadline)
                }
            }
        default:
            Card {
                VStack(alignment: .leading, spacing: 10) {
                    HStack {
                        Image(systemName: "arrow.down.circle.fill").foregroundStyle(Color.brand)
                        Text("Incoming").font(.headline)
                        Spacer()
                        if case .receiving(let band, let rate, _, _) = model.status.activity {
                            ProfileTag(band: band, rate: rate)
                        }
                    }
                    if let t {
                        ProgressView(value: Double(min(t.have, t.needed)), total: Double(max(t.needed, 1))).tint(.brand)
                        HStack {
                            Text("\(min(t.have, t.needed))/\(t.needed) blocks · \(Format.bytes(t.totalLen))")
                            Spacer()
                            if t.lost > 0 {
                                Label("\(t.lost) lost", systemImage: "bandage")
                                    .foregroundStyle(.orange)
                            }
                        }
                        .font(.caption.monospacedDigit())
                        .foregroundStyle(.secondary)
                    }
                    if case .receiving(_, _, _, let progress) = model.status.activity {
                        HStack(spacing: 8) {
                            Text("Packet").font(.caption2).foregroundStyle(.secondary)
                            ProgressView(value: Double(progress)).tint(.brand2)
                        }
                    }
                }
            }
            .transition(.opacity.combined(with: .move(edge: .top)))
        }
    }

    private var micDeniedCard: some View {
        Card {
            VStack(alignment: .leading, spacing: 8) {
                Label("Microphone access is off", systemImage: "mic.slash.fill").font(.headline)
                Text("Allow it in Settings to receive transfers.").font(.subheadline).foregroundStyle(.secondary)
                Button("Open Settings") {
                    if let url = URL(string: UIApplication.openSettingsURLString) { UIApplication.shared.open(url) }
                }
                .buttonStyle(.borderedProminent)
            }
        }
    }

    private var summary: some View {
        let ok = model.received.filter { $0.ok && !$0.incomplete }
        let bytes = ok.reduce(0) { $0 + $1.payloadBytes }
        let best = ok.map(\.speedBPS).max() ?? 0
        return HStack(spacing: 10) {
            StatTile(title: "Received", value: "\(ok.count)/\(model.received.count)", symbol: "tray.and.arrow.down")
            StatTile(title: "Data", value: Format.bytes(bytes), symbol: "shippingbox")
            StatTile(title: "Top speed", value: Format.speed(best), symbol: "speedometer")
        }
    }

    @ViewBuilder private var feed: some View {
        if model.received.isEmpty {
            VStack(spacing: 8) {
                Image(systemName: "tray").font(.largeTitle).foregroundStyle(.tertiary)
                Text("Nothing received yet").font(.headline)
                Text("Start listening here, then send from the other phone.")
                    .font(.subheadline).foregroundStyle(.secondary).multilineTextAlignment(.center)
            }
            .padding(.vertical, 32)
        } else {
            Card {
                VStack(alignment: .leading, spacing: 8) {
                    SectionTitle(text: "Received")
                    ForEach(model.received) { t in
                        NavigationLink(value: t) {
                            TransferRow(transfer: t)
                                .background(model.justReceived == t.id ? Color.brand.opacity(0.08) : .clear)
                        }
                        .buttonStyle(.plain)
                        if t.id != model.received.last?.id { Divider() }
                    }
                }
            }
        }
    }
}
