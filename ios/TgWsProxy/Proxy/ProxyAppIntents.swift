import AppIntents

/// Shortcuts entry points.
///
/// They exist so an automation can start the proxy when Telegram opens and stop
/// it when Telegram is closed, which keeps the background audio session — and
/// therefore the battery cost — limited to the time it is actually needed.
/// The pattern comes from the MIT licensed port by mIwr.

/// Starts the proxy without bringing the app to the front.
struct StartProxyIntent: AudioPlaybackIntent {
    static var title: LocalizedStringResource = "Start TG WS Proxy in the background"
    static var description = IntentDescription(
        "Starts the local MTProto proxy without opening the app."
    )
    static var openAppWhenRun: Bool = false

    @MainActor
    func perform() async throws -> some IntentResult & ReturnsValue<Bool> {
        .result(value: await ProxyController.shared.startAndWait())
    }
}

/// Starts the proxy and opens the app. Use it as the fallback branch when the
/// background variant returns `false`, which happens when iOS refuses to launch
/// the process for an audio intent.
struct StartProxyInAppIntent: AudioPlaybackIntent {
    static var title: LocalizedStringResource = "Start TG WS Proxy"
    static var description = IntentDescription(
        "Opens the app and starts the local MTProto proxy."
    )
    static var openAppWhenRun: Bool = true

    @MainActor
    func perform() async throws -> some IntentResult & ReturnsValue<Bool> {
        .result(value: await ProxyController.shared.startAndWait())
    }
}

struct StopProxyIntent: AudioPlaybackIntent {
    static var title: LocalizedStringResource = "Stop TG WS Proxy"
    static var description = IntentDescription("Stops the local MTProto proxy.")
    static var openAppWhenRun: Bool = false

    @MainActor
    func perform() async throws -> some IntentResult & ReturnsValue<Bool> {
        .result(value: await ProxyController.shared.stopAndWait())
    }
}

struct ProxyShortcuts: AppShortcutsProvider {
    static var appShortcuts: [AppShortcut] {
        AppShortcut(
            intent: StartProxyIntent(),
            phrases: ["Start \(.applicationName) in the background"],
            shortTitle: "Start in the background",
            systemImageName: "play.circle"
        )
        AppShortcut(
            intent: StartProxyInAppIntent(),
            phrases: ["Start \(.applicationName)"],
            shortTitle: "Start",
            systemImageName: "play.fill"
        )
        AppShortcut(
            intent: StopProxyIntent(),
            phrases: ["Stop \(.applicationName)"],
            shortTitle: "Stop",
            systemImageName: "stop.fill"
        )
    }
}
