import SwiftUI

/// Measurement tools: play the channel probe, capture raw audio for desktop analysis.
struct LabView: View {
    @EnvironmentObject var model: AppModel

    var body: some View {
        NavigationStack {
            List {
                Section {
                    probeRow
                } header: {
                    Text("Channel probe")
                } footer: {
                    Text("Plays a ~54 s measurement signal (sweeps, test tones and one test frame per profile). Record it on the other phone with “Record raw audio”, then AirDrop the WAV to a Mac and run `tonesend probe analyze -i recording.wav`.")
                }

                Section {
                    rawRow
                    if let url = model.rawFile {
                        ShareLink(item: url) { Label(url.lastPathComponent, systemImage: "square.and.arrow.up") }
                    }
                } header: {
                    Text("Raw recording")
                } footer: {
                    Text("Saves unprocessed microphone audio (48 kHz, mono WAV).")
                }

                Section("Listen for") {
                    ForEach(BandProfile.allCases) { b in
                        Toggle(isOn: Binding(
                            get: { model.listenBands.contains(b) },
                            set: { on in
                                if on { model.listenBands.insert(b) }
                                else if model.listenBands.count > 1 { model.listenBands.remove(b) }
                            })) {
                            Label { VStack(alignment: .leading) { Text(b.title); Text(b.detail).font(.caption).foregroundStyle(.secondary) } }
                                icon: { Image(systemName: b.symbol) }
                        }
                    }
                }

                Section("Link rate per profile (error-free, large file)") {
                    ForEach(BandProfile.allCases) { b in
                        HStack {
                            Label(b.title, systemImage: b.symbol)
                            Spacer()
                            ForEach(RateProfile.allCases) { r in
                                Text(String(format: "%.1f", Modem.bitrate(b, r) / 1000))
                                    .font(.caption.monospacedDigit())
                                    .frame(width: 40, alignment: .trailing)
                                    .foregroundStyle(r == .fast ? .primary : .secondary)
                            }
                        }
                    }
                    Text("kbit/s for Safe · Normal · Fast").font(.caption).foregroundStyle(.secondary)
                }
            }
            .navigationTitle("Lab")
        }
    }

    @ViewBuilder private var probeRow: some View {
        if let start = model.probePlaying {
            TimelineView(.periodic(from: start, by: 0.1)) { ctx in
                let t = ctx.date.timeIntervalSince(start)
                VStack(alignment: .leading) {
                    HStack {
                        Label("Playing probe…", systemImage: "waveform")
                        Spacer()
                        Button("Stop") { model.stopProbe() }
                    }
                    ProgressView(value: min(1, t / 53.5))
                }
            }
        } else {
            Button { model.playProbe() } label: { Label("Play probe signal", systemImage: "play.circle.fill") }
        }
    }

    @ViewBuilder private var rawRow: some View {
        Button { model.toggleRawRecording() } label: {
            HStack {
                Label(model.rawRecording ? "Stop recording" : "Record raw audio",
                      systemImage: model.rawRecording ? "stop.circle.fill" : "record.circle")
                    .foregroundStyle(model.rawRecording ? .red : .accentColor)
                Spacer()
                if let start = model.rawRecordingStart {
                    TimelineView(.periodic(from: start, by: 0.5)) { ctx in
                        Text(Format.seconds(ctx.date.timeIntervalSince(start))).monospacedDigit().foregroundStyle(.secondary)
                    }
                }
            }
        }
    }
}
