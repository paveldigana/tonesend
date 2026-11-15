import SwiftUI

extension Color {
    static let brand = Color.accentColor
    static let brand2 = Color(red: 0.12, green: 0.75, blue: 0.85)
}

extension LinearGradient {
    static let brand = LinearGradient(colors: [.brand, .brand2], startPoint: .topLeading, endPoint: .bottomTrailing)
}

/// Rounded material card used across screens.
struct Card<Content: View>: View {
    var padding: CGFloat = 16
    @ViewBuilder var content: Content

    var body: some View {
        content
            .padding(padding)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(Color(.secondarySystemGroupedBackground), in: RoundedRectangle(cornerRadius: 20, style: .continuous))
    }
}

struct SectionTitle: View {
    var text: String
    var trailing: String? = nil

    var body: some View {
        HStack {
            Text(text).font(.headline)
            Spacer()
            if let trailing {
                Text(trailing).font(.subheadline).foregroundStyle(.secondary)
            }
        }
    }
}

/// Small labelled metric.
struct StatTile: View {
    var title: String
    var value: String
    var symbol: String
    var tint: Color = .brand

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            Label(title, systemImage: symbol)
                .font(.caption)
                .foregroundStyle(.secondary)
            Text(value)
                .font(.system(.title3, design: .rounded).weight(.semibold))
                .foregroundStyle(tint == .brand ? .primary : tint)
                .lineLimit(1)
                .minimumScaleFactor(0.6)
        }
        .padding(12)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(Color(.secondarySystemGroupedBackground), in: RoundedRectangle(cornerRadius: 14, style: .continuous))
    }
}

/// Capsule tag like "Fast · DQPSK ¾".
struct ProfileTag: View {
    var band: BandProfile
    var rate: RateProfile

    var body: some View {
        Label("\(band.title) · \(rate.title)", systemImage: band.symbol)
            .font(.caption.weight(.medium))
            .padding(.horizontal, 8)
            .padding(.vertical, 4)
            .background(Color.brand.opacity(0.12), in: Capsule())
            .foregroundStyle(Color.brand)
    }
}

/// Horizontal input level bar (dBFS).
struct LevelBar: View {
    var levelDB: Float

    var body: some View {
        GeometryReader { geo in
            let f = CGFloat(max(0, min(1, (levelDB + 70) / 70)))
            ZStack(alignment: .leading) {
                Capsule().fill(.quaternary)
                Capsule()
                    .fill(LinearGradient(colors: [.green, .yellow, .orange, .red],
                                         startPoint: .leading, endPoint: .trailing))
                    .mask(alignment: .leading) {
                        Capsule().frame(width: geo.size.width * f)
                    }
            }
        }
        .frame(height: 8)
        .animation(.linear(duration: 0.1), value: levelDB)
    }
}

/// Pulsing rings around a central button, driven by the input level.
struct ListenOrb: View {
    var active: Bool
    var levelDB: Float
    var receiving: Bool
    var action: () -> Void

    @State private var pulse = false

    var body: some View {
        let level = CGFloat(max(0, min(1, (levelDB + 60) / 50)))
        ZStack {
            ForEach(0..<3) { i in
                Circle()
                    .stroke(Color.brand.opacity(active ? 0.35 - Double(i) * 0.1 : 0), lineWidth: 2)
                    .scaleEffect(active ? 1 + CGFloat(i + 1) * 0.12 + level * 0.25 : 1)
                    .animation(.easeOut(duration: 0.15), value: level)
            }
            Circle()
                .fill(active ? AnyShapeStyle(LinearGradient.brand) : AnyShapeStyle(Color.secondary.opacity(0.25)))
                .shadow(color: .brand.opacity(active ? 0.5 : 0), radius: 18)
                .scaleEffect(receiving && pulse ? 1.06 : 1)
            Image(systemName: active ? (receiving ? "waveform" : "ear.fill") : "ear")
                .font(.system(size: 40, weight: .semibold))
                .foregroundStyle(active ? .white : .secondary)
                .symbolEffect(.variableColor.iterative, isActive: receiving)
        }
        .frame(width: 150, height: 150)
        .contentShape(Circle())
        .onTapGesture(perform: action)
        .onAppear {
            withAnimation(.easeInOut(duration: 0.6).repeatForever(autoreverses: true)) { pulse = true }
        }
        .accessibilityLabel(active ? "Stop listening" : "Start listening")
        .accessibilityAddTraits(.isButton)
    }
}

/// Row in a transfer feed.
struct TransferRow: View {
    var transfer: Transfer

    var body: some View {
        HStack(spacing: 12) {
            thumbnail
            VStack(alignment: .leading, spacing: 4) {
                Text(transfer.title)
                    .font(.subheadline.weight(.medium))
                    .lineLimit(2)
                HStack(spacing: 6) {
                    Text(Format.bytes(transfer.payloadBytes))
                    Text("·")
                    if !transfer.incomplete { Text(Format.speed(transfer.speedBPS)).monospacedDigit() }
                    Text("·")
                    Text(Format.seconds(transfer.airTime))
                }
                .font(.caption)
                .foregroundStyle(.secondary)
            }
            Spacer(minLength: 0)
            VStack(alignment: .trailing, spacing: 4) {
                if transfer.timesHeard > 1 {
                    Text("heard \(transfer.timesHeard)×")
                        .font(.caption2.weight(.semibold))
                        .padding(.horizontal, 6).padding(.vertical, 2)
                        .background(Color.brand.opacity(0.12), in: Capsule())
                        .foregroundStyle(Color.brand)
                        .contentTransition(.numericText())
                }
                if transfer.direction == .received {
                    Image(systemName: transfer.ok ? "checkmark.seal.fill" : "exclamationmark.triangle.fill")
                        .foregroundStyle(transfer.ok ? .green : .orange)
                } else {
                    Image(systemName: "arrow.up.circle.fill").foregroundStyle(Color.brand)
                }
                Text(transfer.date, style: .time)
                    .font(.caption2)
                    .foregroundStyle(.tertiary)
            }
        }
        .padding(.vertical, 4)
    }

    @ViewBuilder private var thumbnail: some View {
        if let img = transfer.content.image {
            Image(uiImage: img)
                .resizable()
                .scaledToFill()
                .frame(width: 44, height: 44)
                .clipShape(RoundedRectangle(cornerRadius: 10, style: .continuous))
        } else {
            Image(systemName: transfer.content.kindSymbol)
                .font(.title3)
                .foregroundStyle(Color.brand)
                .frame(width: 44, height: 44)
                .background(Color.brand.opacity(0.12), in: RoundedRectangle(cornerRadius: 10, style: .continuous))
        }
    }
}
