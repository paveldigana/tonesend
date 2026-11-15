import SwiftUI

@main
struct ToneSendApp: App {
    @StateObject private var model = AppModel()
    @AppStorage("tab") private var tab = 0

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
            .alert("Error", isPresented: Binding(get: { model.lastError != nil }, set: { if !$0 { model.lastError = nil } })) {
                Button("OK", role: .cancel) {}
            } message: {
                Text(model.lastError ?? "")
            }
        }
    }
}
