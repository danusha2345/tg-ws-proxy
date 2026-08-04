import SwiftUI

struct ContentView: View {
    @Binding var tab: AppTab

    var body: some View {
        TabView(selection: $tab) {
            HomeView()
                .tabItem { Label("Proxy", systemImage: "bolt.horizontal.circle") }
                .tag(AppTab.home)
            SettingsView()
                .tabItem { Label("Settings", systemImage: "gearshape") }
                .tag(AppTab.settings)
            LogsView()
                .tabItem { Label("Logs", systemImage: "doc.plaintext") }
                .tag(AppTab.logs)
            InfoView()
                .tabItem { Label("About", systemImage: "info.circle") }
                .tag(AppTab.info)
        }
    }
}

struct HomeView: View {
    @EnvironmentObject private var controller: ProxyController

    var body: some View {
        NavigationStack {
            ScrollView {
                VStack(spacing: 16) {
                    statusCard
                    if let message = controller.errorMessage {
                        errorCard(message)
                    }
                    toggleButton
                    telegramButtons
                    backgroundNote
                }
                .padding()
            }
            .navigationTitle("TG WS Proxy")
        }
    }

    private var statusCard: some View {
        VStack(alignment: .leading, spacing: 12) {
            HStack {
                Circle()
                    .fill(stateColor)
                    .frame(width: 12, height: 12)
                Text(stateTitle)
                    .font(.headline)
                Spacer()
                if controller.status.isRunning {
                    Text(uptime)
                        .font(.subheadline.monospacedDigit())
                        .foregroundStyle(.secondary)
                }
            }

            Divider()

            Grid(alignment: .leading, horizontalSpacing: 16, verticalSpacing: 6) {
                counterRow("Active", controller.status.activeConnections)
                counterRow("Total", controller.status.totalConnections)
                counterRow("WebSocket", controller.status.websocketConnections)
                counterRow("TCP fallback", controller.status.tcpFallbackConnections)
                counterRow("Cloudflare", controller.status.cloudflareConnections)
                counterRow("Rejected", controller.status.badConnections)
            }
            .font(.subheadline)

            Divider()

            HStack {
                Label(Formatting.bytes(controller.status.bytesUp), systemImage: "arrow.up")
                Spacer()
                Label(Formatting.bytes(controller.status.bytesDown), systemImage: "arrow.down")
            }
            .font(.subheadline.monospacedDigit())
            .foregroundStyle(.secondary)
        }
        .padding()
        .background(Color(uiColor: .secondarySystemBackground), in: RoundedRectangle(cornerRadius: 16))
    }

    private func counterRow(_ title: LocalizedStringKey, _ value: UInt64) -> some View {
        GridRow {
            Text(title).foregroundStyle(.secondary)
            Text(String(value)).monospacedDigit()
        }
    }

    private func errorCard(_ message: String) -> some View {
        HStack(alignment: .top, spacing: 8) {
            Image(systemName: "exclamationmark.triangle.fill")
                .foregroundStyle(.orange)
            Text(message)
                .font(.footnote)
                .fixedSize(horizontal: false, vertical: true)
            Spacer(minLength: 0)
        }
        .padding()
        .background(Color(uiColor: .secondarySystemBackground), in: RoundedRectangle(cornerRadius: 12))
    }

    private var toggleButton: some View {
        Button(action: controller.toggle) {
            Text(controller.status.isActive ? "Stop proxy" : "Start proxy")
                .frame(maxWidth: .infinity)
                .padding(.vertical, 6)
        }
        .buttonStyle(.borderedProminent)
        .controlSize(.large)
        .tint(controller.status.isActive ? .red : .accentColor)
        .disabled(controller.isBusy)
    }

    private var telegramButtons: some View {
        VStack(spacing: 10) {
            Button {
                controller.openTelegram()
            } label: {
                Label("Connect Telegram", systemImage: "paperplane.fill")
                    .frame(maxWidth: .infinity)
            }
            .buttonStyle(.bordered)
            .controlSize(.large)

            Button {
                controller.copyTelegramLink()
            } label: {
                Label("Copy tg://proxy link", systemImage: "doc.on.doc")
                    .frame(maxWidth: .infinity)
            }
            .buttonStyle(.bordered)
            .controlSize(.large)
        }
    }

    private var backgroundNote: some View {
        Text("""
            While the proxy runs the app plays silent audio so iOS keeps it \
            alive after you switch to Telegram. Force quitting the app from the \
            app switcher stops the proxy.
            """)
            .font(.footnote)
            .foregroundStyle(.secondary)
            .frame(maxWidth: .infinity, alignment: .leading)
    }

    private var stateTitle: LocalizedStringKey {
        switch controller.status.state {
        case "running": return "Running"
        case "starting": return "Starting"
        case "stopping": return "Stopping"
        case "failed": return "Failed"
        default: return "Stopped"
        }
    }

    private var stateColor: Color {
        switch controller.status.state {
        case "running": return .green
        case "starting", "stopping": return .orange
        case "failed": return .red
        default: return .secondary
        }
    }

    private var uptime: String {
        guard let started = controller.status.startedAtEpochSeconds else { return "" }
        let elapsed = max(0, Int(Date().timeIntervalSince1970) - Int(started))
        return Formatting.duration(seconds: elapsed)
    }
}

enum Formatting {
    static func bytes(_ value: UInt64) -> String {
        ByteCountFormatter.string(fromByteCount: Int64(clamping: value), countStyle: .binary)
    }

    static func duration(seconds: Int) -> String {
        let hours = seconds / 3600
        let minutes = (seconds % 3600) / 60
        let remainder = seconds % 60
        if hours > 0 {
            return String(format: "%d:%02d:%02d", hours, minutes, remainder)
        }
        return String(format: "%02d:%02d", minutes, remainder)
    }
}
