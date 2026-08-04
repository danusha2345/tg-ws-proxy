import SwiftUI

struct InfoView: View {
    var body: some View {
        NavigationStack {
            List {
                Section("How it works") {
                    Text("""
                        The app runs the same Rust core as the desktop and \
                        Android builds. It listens on 127.0.0.1 and forwards \
                        Telegram traffic over a TLS WebSocket, a Cloudflare \
                        Worker or a plain TCP fallback.
                        """)
                    Text("""
                        Point Telegram at the proxy with Connect Telegram, or \
                        enter it manually in Settings → Data and Storage → \
                        Proxy: server 127.0.0.1, the port from Settings and the \
                        secret shown there with a dd prefix.
                        """)
                }

                Section("Background limits") {
                    Text("""
                        iOS has no foreground service. This build keeps the \
                        listener alive with the audio background mode: while the \
                        proxy runs the app loops inaudible silence, which costs \
                        extra battery. Other apps keep playing their own audio.
                        """)
                    Text("""
                        The proxy stops when you force quit the app from the app \
                        switcher, and iOS may still reclaim the process under \
                        memory pressure. A build signed with a paid Apple \
                        Developer account could use a Network Extension instead; \
                        this free build deliberately does not.
                        """)
                }

                Section("Shortcuts") {
                    Text("""
                        Three actions are exposed to Shortcuts: start in the \
                        background, start in the app and stop. Automate them on \
                        "When Telegram is opened" and "When Telegram is closed" \
                        so the audio session — and the battery cost — only lasts \
                        while you actually use Telegram.
                        """)
                    Text("""
                        The background action returns false when iOS refuses to \
                        launch the app for it. Branch on that result and run the \
                        in-app action as the fallback.
                        """)
                }

                Section("Links") {
                    Link(
                        "Project on GitHub",
                        destination: URL(string: "https://github.com/danusha2345/tg-ws-proxy")!
                    )
                    Link(
                        "Report an issue",
                        destination: URL(
                            string: "https://github.com/danusha2345/tg-ws-proxy/issues"
                        )!
                    )
                }

                Section("Credits") {
                    Text("""
                        Original project: Flowseal/tg-ws-proxy. The silent audio \
                        keep-alive and the Shortcuts automation follow the MIT \
                        licensed iOS port by mIwr. Thanks to amurcanov, \
                        IMDelewer and MushroomSquad, whose ports mapped out what \
                        iOS does and does not allow.
                        """)
                    .font(.footnote)
                    .foregroundStyle(.secondary)
                }

                Section {
                    LabeledContent("Version", value: Self.version)
                }
            }
            .navigationTitle("About")
        }
    }

    private static var version: String {
        let info = Bundle.main.infoDictionary
        let short = info?["CFBundleShortVersionString"] as? String ?? "?"
        let build = info?["CFBundleVersion"] as? String ?? "?"
        return "\(short) (\(build))"
    }
}
