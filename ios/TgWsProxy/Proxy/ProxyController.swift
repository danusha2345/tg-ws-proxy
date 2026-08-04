import Foundation
import UIKit

/// Owns the proxy lifecycle: validates the settings, drives the Rust core and
/// keeps the published status fresh for the views.
@MainActor
final class ProxyController: ObservableObject {
    static let shared = ProxyController()

    @Published private(set) var status = ProxyStatus()
    @Published private(set) var isBusy = false
    @Published var settings: ProxySettings
    @Published var errorMessage: String?

    private let store = ProxySettingsStore()
    private var pollTimer: Timer?

    private init() {
        settings = store.load()
        status = NativeBridge.status()
        if status.isActive {
            // The app was relaunched while the core kept running in this process.
            startPolling()
        }
    }

    /// Nil when the Keychain refuses to hand the secret over, which is a
    /// transient state rather than a reason to mint a new one.
    var secret: String? { try? store.secret() }

    /// The link handed to Telegram. While the core runs it reports the value it
    /// actually bound, otherwise it is composed from the pending settings.
    var telegramURL: URL? {
        if let reported = status.telegramUrl, let url = URL(string: reported) {
            return url
        }
        guard let secret else { return nil }
        return URL(string: "tg://proxy?server=127.0.0.1&port=\(settings.port)&secret=dd\(secret)")
    }

    var canEditSettings: Bool { !status.isActive }

    // MARK: - Lifecycle

    func start() {
        Task { _ = await startAndWait() }
    }

    func stop() {
        Task { _ = await stopAndWait() }
    }

    func toggle() {
        if status.isActive { stop() } else { start() }
    }

    /// Starts the core and reports whether it is listening. Used by the App
    /// Intents so a Shortcut can branch on the result.
    @discardableResult
    func startAndWait() async -> Bool {
        guard !isBusy else { return status.isActive }
        guard !status.isActive else { return true }
        errorMessage = nil

        if let problem = validationProblem() {
            errorMessage = problem
            return false
        }

        let configurationJSON: String
        do {
            configurationJSON = try store.configurationJSON(for: settings)
        } catch {
            errorMessage = String(
                localized: "Could not build the configuration: \(error.localizedDescription)"
            )
            return false
        }

        isBusy = true
        AppLog.shared.append("start requested on port \(settings.port)")
        let response = await Task.detached(priority: .userInitiated) {
            NativeBridge.start(configurationJSON: configurationJSON)
        }.value
        finishStart(response)
        return response.ok
    }

    @discardableResult
    func stopAndWait() async -> Bool {
        guard !isBusy, status.isActive else { return !status.isActive }
        isBusy = true
        AppLog.shared.append("stop requested")
        let response = await Task.detached(priority: .userInitiated) {
            NativeBridge.stop()
        }.value
        finishStop(response)
        return response.ok
    }

    private func finishStart(_ response: NativeResponse) {
        isBusy = false
        guard response.ok else {
            errorMessage = response.error ?? String(localized: "The core refused to start.")
            AppLog.shared.append("start failed: \(errorMessage ?? "")")
            refresh()
            return
        }
        if !KeepAliveController.shared.start() {
            errorMessage = String(localized: """
                The proxy started, but background audio is unavailable. \
                It will stop when the app leaves the screen.
                """)
        }
        startPolling()
        refresh()
    }

    private func finishStop(_ response: NativeResponse) {
        isBusy = false
        if !response.ok {
            errorMessage = response.error
            AppLog.shared.append("stop failed: \(response.error ?? "")")
        }
        refresh()
    }

    // MARK: - Status polling

    private func startPolling() {
        guard pollTimer == nil else { return }
        let timer = Timer(timeInterval: 1, repeats: true) { _ in
            Task { @MainActor in ProxyController.shared.refresh() }
        }
        RunLoop.main.add(timer, forMode: .common)
        pollTimer = timer
    }

    private func stopPolling() {
        pollTimer?.invalidate()
        pollTimer = nil
    }

    func refresh() {
        let fresh = NativeBridge.status()
        status = fresh

        guard !fresh.isActive else {
            // The audio session can die without posting any of the
            // notifications KeepAliveController listens for; this is the
            // watchdog that notices and re-arms it.
            KeepAliveController.shared.ensureRunning()
            return
        }
        stopPolling()
        KeepAliveController.shared.stop()
        if fresh.state == "failed", let error = fresh.error, errorMessage == nil {
            errorMessage = error
            AppLog.shared.append("core failed: \(error)")
        }
    }

    // MARK: - Settings

    func saveSettings() {
        store.save(settings)
        settings = store.load()
    }

    /// Returns the error to show, or nil on success.
    func regenerateSecret() -> String? {
        do {
            _ = try store.regenerateSecret()
            objectWillChange.send()
            return nil
        } catch {
            return error.localizedDescription
        }
    }

    func replaceSecret(with secret: String) -> String? {
        guard ProxyInputValidator.validSecret(secret) else {
            return String(localized: "The secret must be exactly 32 hex characters.")
        }
        do {
            try store.replaceSecret(with: secret)
            objectWillChange.send()
            return nil
        } catch {
            return error.localizedDescription
        }
    }

    /// Returns the first user-visible reason the settings cannot be applied.
    private func validationProblem() -> String? {
        if !ProxyInputValidator.validPort(settings.port) {
            return String(localized: "The port must be between 1 and 65535.")
        }
        if !ProxyInputValidator.validPoolSize(settings.poolSize) {
            return String(localized: "The pool size must be between 0 and 128.")
        }
        if !ProxyInputValidator.validDomains(settings.workerDomains) {
            return String(localized: "The Worker domain list contains an invalid host name.")
        }
        let fakeTls = settings.fakeTlsDomain.trimmingCharacters(in: .whitespacesAndNewlines)
        let masking = settings.maskingUpstream.trimmingCharacters(in: .whitespacesAndNewlines)
        if !fakeTls.isEmpty, !ProxyInputValidator.validDomain(fakeTls.lowercased()) {
            return String(localized: "The Fake TLS domain is not a valid host name.")
        }
        if !masking.isEmpty, !ProxyInputValidator.validDomain(masking.lowercased()) {
            return String(localized: "The masking upstream is not a valid host name.")
        }
        if !masking.isEmpty, fakeTls.isEmpty {
            return String(localized: "The masking upstream requires a Fake TLS domain.")
        }
        if !masking.isEmpty, masking.lowercased() == fakeTls.lowercased() {
            return String(localized: "The Fake TLS domain and the masking upstream must differ.")
        }
        do {
            let stored = try store.secret()
            guard ProxyInputValidator.validSecret(stored) else {
                return String(localized: "The stored secret is malformed. Generate a new one.")
            }
        } catch {
            return error.localizedDescription
        }
        return nil
    }

    // MARK: - Telegram

    func openTelegram() {
        guard let url = telegramURL else { return }
        UIApplication.shared.open(url)
    }

    func copyTelegramLink() {
        guard let url = telegramURL else { return }
        UIPasteboard.general.string = url.absoluteString
    }
}
