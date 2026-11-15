import PhotosUI
import SwiftUI
import UniformTypeIdentifiers

struct SendView: View {
    @EnvironmentObject var model: AppModel

    enum Mode: String, CaseIterable, Identifiable {
        case text = "Text", photo = "Photo", file = "File"
        var id: String { rawValue }
        var symbol: String {
            switch self {
            case .text: "text.alignleft"
            case .photo: "photo"
            case .file: "doc"
            }
        }
    }

    @AppStorage("send.band") private var band: BandProfile = .max
    @AppStorage("send.rate") private var rate: RateProfile = .fast
    @AppStorage("send.photoSize") private var photoSize: Double = 240
    @AppStorage("send.redundancy") private var redundancy: Double = 0.25
    @State private var mode: Mode = .text
    @State private var text = "Hello from ToneSend 👋"
    @State private var photoItem: PhotosPickerItem?
    @State private var photo: UIImage?
    /// WebP produced by the framework, its decoded preview (exactly what is sent) and size.
    @State private var photoData: Data?
    @State private var photoPreview: UIImage?
    @State private var photoPixels: (Int, Int)?
    /// Packing result of the current content (computed off the main thread).
    @State private var packInfo: PackInfo?
    @State private var file: (name: String, data: Data)?
    @State private var importing = false
    @FocusState private var editing: Bool

    var body: some View {
        NavigationStack {
            ScrollView {
                VStack(spacing: 16) {
                    contentCard
                    profileCard
                    estimateCard
                    sendButton
                    if !model.sent.isEmpty { historyCard }
                    tip
                }
                .padding()
            }
            .scrollDismissesKeyboard(.interactively)
            .background(Color(.systemGroupedBackground))
            .navigationTitle("Send")
            .toolbar {
                ToolbarItemGroup(placement: .keyboard) {
                    Spacer()
                    Button("Done") { editing = false }
                }
            }
            .fileImporter(isPresented: $importing, allowedContentTypes: [.item]) { result in
                guard case .success(let url) = result else { return }
                let scoped = url.startAccessingSecurityScopedResource()
                defer { if scoped { url.stopAccessingSecurityScopedResource() } }
                if let data = try? Data(contentsOf: url) { file = (url.lastPathComponent, data) }
            }
            .onChange(of: photoItem) { _, item in loadPhoto(item) }
            .onChange(of: photoSize) { _, _ in recompressPhoto() }
            .task(id: contentKey) { await updatePackInfo() }
        }
    }

    /// Changes whenever the content to send changes.
    private var contentKey: String {
        switch mode {
        case .text: "t\(text.hashValue)"
        case .photo: "p\(photoData?.count ?? 0)-\(photoData?.hashValue ?? 0)"
        case .file: "f\(file?.name ?? "")-\(file?.data.count ?? 0)"
        }
    }

    private func updatePackInfo() async {
        guard let content else { packInfo = nil; return }
        try? await Task.sleep(for: .milliseconds(200)) // debounce typing
        if Task.isCancelled { return }
        let info = await Task.detached(priority: .userInitiated) { content.packed().1 }.value
        if !Task.isCancelled { packInfo = info }
    }

    // MARK: content

    private var content: TransferContent? {
        switch mode {
        case .text: text.isEmpty ? nil : .text(text)
        case .photo: photoData.map { .image(name: "photo.webp", data: $0) }
        case .file: file.map { .file(name: $0.name, data: $0.data) }
        }
    }

    private var contentCard: some View {
        Card {
            VStack(alignment: .leading, spacing: 12) {
                Picker("Content", selection: $mode) {
                    ForEach(Mode.allCases) { m in Label(m.rawValue, systemImage: m.symbol).tag(m) }
                }
                .pickerStyle(.segmented)

                switch mode {
                case .text:
                    TextEditor(text: $text)
                        .focused($editing)
                        .frame(minHeight: 110)
                        .scrollContentBackground(.hidden)
                        .padding(8)
                        .background(Color(.tertiarySystemFill), in: RoundedRectangle(cornerRadius: 12))
                case .photo:
                    photoPicker
                case .file:
                    filePicker
                }
            }
        }
    }

    private var photoPicker: some View {
        VStack(spacing: 10) {
            PhotosPicker(selection: $photoItem, matching: .images) {
                Group {
                    if let shown = photoPreview ?? photo {
                        Image(uiImage: shown).resizable().scaledToFit()
                            .frame(maxHeight: 180)
                            .clipShape(RoundedRectangle(cornerRadius: 12))
                    } else {
                        Label("Choose a photo", systemImage: "photo.badge.plus")
                            .frame(maxWidth: .infinity, minHeight: 110)
                            .background(Color(.tertiarySystemFill), in: RoundedRectangle(cornerRadius: 12))
                    }
                }
            }
            if photo != nil {
                VStack(alignment: .leading) {
                    HStack {
                        Text("Resolution")
                        Spacer()
                        Text("\(photoPixels.map { "\($0.0)×\($0.1)" } ?? "\(Int(photoSize)) px") · WebP \(Format.bytes(photoData?.count ?? 0))")
                            .foregroundStyle(.secondary).monospacedDigit()
                    }
                    .font(.caption)
                    Slider(value: $photoSize, in: 96...640, step: 16)
                }
            }
        }
    }

    private var filePicker: some View {
        Button { importing = true } label: {
            HStack {
                Image(systemName: file == nil ? "doc.badge.plus" : "doc.fill").font(.title2)
                VStack(alignment: .leading) {
                    Text(file?.name ?? "Choose a file")
                    if let file { Text(Format.bytes(file.data.count)).font(.caption).foregroundStyle(.secondary) }
                }
                Spacer()
            }
            .padding()
            .frame(maxWidth: .infinity, minHeight: 80)
            .background(Color(.tertiarySystemFill), in: RoundedRectangle(cornerRadius: 12))
        }
        .buttonStyle(.plain)
    }

    // MARK: profile

    private var profileCard: some View {
        Card {
            VStack(alignment: .leading, spacing: 12) {
                SectionTitle(text: "Sound profile")
                LazyVGrid(columns: [GridItem(.flexible(), spacing: 10), GridItem(.flexible(), spacing: 10)], spacing: 10) {
                    ForEach(BandProfile.allCases) { b in bandTile(b) }
                }
                Picker("Speed", selection: $rate) {
                    ForEach(RateProfile.allCases) { r in Text(r.title).tag(r) }
                }
                .pickerStyle(.segmented)
                Text("\(band.detail) · \(rate.detail)")
                    .font(.caption)
                    .foregroundStyle(.secondary)
                Divider()
                HStack {
                    Label("Repair data", systemImage: "cross.case")
                        .font(.subheadline)
                    Spacer()
                    Picker("Repair data", selection: $redundancy) {
                        Text("Off").tag(0.0)
                        Text("10 %").tag(0.1)
                        Text("25 %").tag(0.25)
                        Text("50 %").tag(0.5)
                    }
                    .pickerStyle(.segmented)
                    .frame(maxWidth: 220)
                }
                Text(redundancy > 0
                     ? "Up to \(Int(redundancy * 100)) % of lost blocks are rebuilt automatically. Every block and the whole file are checksummed."
                     : "No automatic repair. Lost blocks can still be fixed later with “Send repair data”.")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
        }
    }

    private func bandTile(_ b: BandProfile) -> some View {
        let selected = b == band
        return Button {
            withAnimation(.snappy) { band = b }
        } label: {
            VStack(spacing: 6) {
                Image(systemName: b.symbol).font(.title3)
                Text(b.title).font(.subheadline.weight(.semibold))
                Text(Format.speed(Modem.bitrate(b, rate)))
                    .font(.caption2).monospacedDigit()
                    .opacity(0.8)
            }
            .frame(maxWidth: .infinity)
            .padding(.vertical, 12)
            .foregroundStyle(selected ? .white : .primary)
            .background {
                RoundedRectangle(cornerRadius: 14, style: .continuous)
                    .fill(selected ? AnyShapeStyle(LinearGradient.brand) : AnyShapeStyle(Color(.tertiarySystemFill)))
            }
        }
        .buttonStyle(.plain)
    }

    // MARK: estimate + send

    private var estimateCard: some View {
        let bytes = packInfo?.packedLen ?? 0
        let original = content?.contentBytes ?? 0
        let air = Modem.airTime(band, rate, bytes: bytes, redundancy: Float(redundancy))
        return VStack(alignment: .leading, spacing: 8) {
            HStack(spacing: 10) {
                StatTile(title: "On air", value: Format.bytes(bytes), symbol: "shippingbox")
                StatTile(title: "Air time", value: Format.seconds(air), symbol: "timer")
                // Effective speed: content delivered per second (compression makes it higher).
                StatTile(title: "Speed", value: Format.speed(air > 0 ? Double(original * 8) / air : 0), symbol: "speedometer")
            }
            if let info = packInfo {
                Label(compressionNote(info), systemImage: "arrow.down.right.and.arrow.up.left")
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .padding(.horizontal, 4)
            }
        }
    }

    private func compressionNote(_ i: PackInfo) -> String {
        if mode == .photo, let px = photoPixels {
            return "Photo \(px.0)×\(px.1) encoded as WebP by the framework (\(Format.bytes(i.packedLen)) on air)"
        }
        if i.compressed {
            return "Compressed \(Format.bytes(i.originalLen)) → \(Format.bytes(i.packedLen)) (−\(Int((i.saving * 100).rounded())) %, Brotli)"
        }
        return "Not compressible — sent as is (\(Format.bytes(i.packedLen)))"
    }

    @ViewBuilder private var sendButton: some View {
        if let s = model.sending {
            Card {
                TimelineView(.periodic(from: s.start, by: 0.05)) { ctx in
                    let elapsed = ctx.date.timeIntervalSince(s.start)
                    let progress = min(1, elapsed / max(s.duration, 0.01))
                    VStack(alignment: .leading, spacing: 10) {
                        HStack {
                            Image(systemName: "waveform").symbolEffect(.variableColor.iterative)
                            Text(s.repair ? "Sending repair data…" : "Sending…").font(.headline)
                            Spacer()
                            Text("\(Format.seconds(elapsed)) / \(Format.seconds(s.duration))")
                                .font(.subheadline).monospacedDigit().foregroundStyle(.secondary)
                        }
                        ProgressView(value: progress).tint(.brand)
                        HStack {
                            Text("\(Format.bytes(Int(Double(s.transfer.payloadBytes) * progress))) of \(Format.bytes(s.transfer.payloadBytes))")
                                .font(.caption).foregroundStyle(.secondary).monospacedDigit()
                            Spacer()
                            Button("Stop", role: .destructive) { model.cancelSend() }
                                .buttonStyle(.bordered)
                        }
                    }
                }
            }
        } else {
            Button {
                editing = false
                if let content { model.send(content, band: band, rate: rate, redundancy: Float(redundancy)) }
            } label: {
                Label("Send as sound", systemImage: "dot.radiowaves.right")
                    .font(.headline)
                    .frame(maxWidth: .infinity)
                    .padding(.vertical, 16)
                    .foregroundStyle(.white)
                    .background(LinearGradient.brand, in: RoundedRectangle(cornerRadius: 18, style: .continuous))
                    .shadow(color: .brand.opacity(0.35), radius: 12, y: 6)
            }
            .disabled(content == nil)
            .opacity(content == nil ? 0.5 : 1)
        }
    }

    private var historyCard: some View {
        Card {
            VStack(alignment: .leading, spacing: 8) {
                SectionTitle(text: "Recently sent", trailing: "\(model.sent.count)")
                ForEach(model.sent.prefix(5)) { t in
                    NavigationLink(value: t) { TransferRow(transfer: t) }
                        .buttonStyle(.plain)
                    if t.id != model.sent.prefix(5).last?.id { Divider() }
                }
            }
        }
        .navigationDestination(for: Transfer.self) { TransferDetailView(transfer: $0) }
    }

    private var tip: some View {
        Label("Turn the volume up and keep the phones 10–50 cm apart, speaker facing the other phone's microphone (bottom edge).",
              systemImage: "lightbulb")
            .font(.footnote)
            .foregroundStyle(.secondary)
            .padding(.horizontal, 4)
    }

    // MARK: photo handling

    private func loadPhoto(_ item: PhotosPickerItem?) {
        Task {
            guard let item, let data = try? await item.loadTransferable(type: Data.self),
                  let img = UIImage(data: data) else { return }
            photo = img
            recompressPhoto()
        }
    }

    /// Resize + WebP happen in the framework, so every platform sends the same thing.
    private func recompressPhoto() {
        guard let photo else { return }
        let side = Int(photoSize)
        Task {
            let r = await Task.detached(priority: .userInitiated) {
                ContentCodec.encodeImage(photo, maxSide: side, quality: 55)
            }.value
            guard let (data, w, h) = r, Int(photoSize) == side else { return }
            photoData = data
            photoPixels = (w, h)
            photoPreview = ContentCodec.image(from: data)
        }
    }
}

