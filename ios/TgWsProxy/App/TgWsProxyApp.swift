import SwiftUI

enum AppTab: Hashable {
    case home, settings, logs, info
}

@main
struct TgWsProxyApp: App {
    @StateObject private var controller = ProxyController.shared
    @State private var tab: AppTab = .home

    var body: some Scene {
        WindowGroup {
            ContentView(tab: $tab)
                .environmentObject(controller)
                .onOpenURL { url in
                    DeepLinkRouter.handle(url, tab: &tab, controller: controller)
                }
        }
    }
}

/// Handles the `tgwsproxy://` scheme.
///
/// Supported links: `tgwsproxy://start`, `tgwsproxy://stop`,
/// `tgwsproxy://home`, `tgwsproxy://settings`, `tgwsproxy://logs`,
/// `tgwsproxy://info`.
enum DeepLinkRouter {
    @MainActor
    static func handle(_ url: URL, tab: inout AppTab, controller: ProxyController) {
        guard url.scheme == "tgwsproxy" else { return }
        // `tgwsproxy://start` parses the action as the host, `tgwsproxy:///start`
        // as the path; accept both.
        let action = url.host ?? url.path.trimmingCharacters(in: CharacterSet(charactersIn: "/"))
        switch action.lowercased() {
        case "start":
            controller.start()
        case "stop":
            controller.stop()
        case "settings":
            tab = .settings
        case "logs":
            tab = .logs
        case "info":
            tab = .info
        default:
            tab = .home
        }
    }
}
