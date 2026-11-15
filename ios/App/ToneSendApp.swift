import SwiftUI

@main
struct ToneSendApp: App {
    @StateObject private var model = AppModel()
    @AppStorage("tab") private var tab = 0
    @Environment(\.scenePhase) private var phase

    var body: some Scene {
        WindowGroup {
            TabView(selection: $tab) {
                SendView()
                    .tabItem { Label("Send", systemImage: "dot.radiowaves.right") }
                    .tag(0)
                ReceiveView()
                    .tabItem { Label("Receive", systemImage: "ear") }
                    .badge(model.received.isEmpty ? 0 : model.received.count)
                    .tag(1)
                LabView()
                    .tabItem { Label("Lab", systemImage: "flask") }
                    .tag(2)
            }
            .environmentObject(model)
            .onChange(of: phase) { _, p in
                let name: String = switch p {
                case .active: "active"
                case .inactive: "inactive"
                case .background: "background"
                @unknown default: "unknown"
                }
                EventLog.shared.log("app_phase", ["phase": name])
            }
            .alert("Error", isPresented: Binding(get: { model.lastError != nil }, set: { if !$0 { model.lastError = nil } })) {
                Button("OK", role: .cancel) {}
            } message: {
                Text(model.lastError ?? "")
            }
        }
    }
}
