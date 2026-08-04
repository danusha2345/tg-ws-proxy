import Foundation
import Security

/// User editable proxy parameters. The secret is kept in the Keychain and is
/// therefore not part of this value.
struct ProxySettings: Equatable {
    var port: Int = 1443
    var poolSize: Int = 4
    var fallbackCfproxy: Bool = true
    var workerDomains: String = ""
    var fakeTlsDomain: String = ""
    var maskingUpstream: String = ""
}

/// Input rules mirrored from the Android client so both clients reject the same
/// values before the core sees them.
enum ProxyInputValidator {
    static func validPort(_ value: Int) -> Bool { (1...65535).contains(value) }

    static func validPoolSize(_ value: Int) -> Bool { (0...128).contains(value) }

    static func validSecret(_ value: String) -> Bool {
        let secret = value.trimmingCharacters(in: .whitespacesAndNewlines)
        return secret.count == 32 && secret.allSatisfy(\.isHexDigit)
    }

    static func parseDomains(_ value: String) -> [String] {
        let separators = CharacterSet(charactersIn: ",; \t\n\r")
        var seen = Set<String>()
        return value
            .components(separatedBy: separators)
            .map { $0.trimmingCharacters(in: .whitespaces).lowercased() }
            .filter { !$0.isEmpty }
            .filter { seen.insert($0).inserted }
    }

    static func validDomains(_ value: String) -> Bool {
        parseDomains(value).allSatisfy(validDomain)
    }

    /// Accepts a dotted host name; an empty string means "not configured" and is
    /// checked by the caller.
    static func validDomain(_ domain: String) -> Bool {
        guard domain.count <= 253, !domain.hasPrefix("."), !domain.hasSuffix(".") else {
            return false
        }
        let labels = domain.split(separator: ".", omittingEmptySubsequences: false)
        guard labels.count >= 2 else { return false }
        return labels.allSatisfy { label in
            guard (1...63).contains(label.count) else { return false }
            guard let first = label.first, let last = label.last,
                  first.isLetter || first.isNumber, last.isLetter || last.isNumber
            else { return false }
            return label.allSatisfy { $0.isLetter || $0.isNumber || $0 == "-" }
        }
    }
}

/// Configuration payload understood by `crates/mobile-core`.
private struct ProxyConfiguration: Encodable {
    let port: Int
    let secret: String
    let poolSize: Int
    let fallbackCfproxy: Bool
    let workerDomains: [String]
    let fakeTlsDomain: String
    let maskingUpstream: String
    let logPath: String
}

/// Persists settings in `UserDefaults` and the MTProto secret in the Keychain.
struct ProxySettingsStore {
    private enum Key {
        static let port = "port"
        static let poolSize = "pool-size"
        static let cfproxy = "cfproxy"
        static let workerDomains = "worker-domains"
        static let fakeTls = "fake-tls"
        static let masking = "masking"
    }

    private let defaults: UserDefaults
    private let keychain = KeychainSecretStore(service: "com.danusha.tgwsproxy.secret")

    init(defaults: UserDefaults = .standard) {
        self.defaults = defaults
    }

    func load() -> ProxySettings {
        var settings = ProxySettings()
        if let port = defaults.object(forKey: Key.port) as? Int { settings.port = port }
        if let poolSize = defaults.object(forKey: Key.poolSize) as? Int {
            settings.poolSize = poolSize
        }
        if let cfproxy = defaults.object(forKey: Key.cfproxy) as? Bool {
            settings.fallbackCfproxy = cfproxy
        }
        settings.workerDomains = defaults.string(forKey: Key.workerDomains) ?? ""
        settings.fakeTlsDomain = defaults.string(forKey: Key.fakeTls) ?? ""
        settings.maskingUpstream = defaults.string(forKey: Key.masking) ?? ""
        return settings
    }

    func save(_ settings: ProxySettings) {
        defaults.set(settings.port, forKey: Key.port)
        defaults.set(settings.poolSize, forKey: Key.poolSize)
        defaults.set(settings.fallbackCfproxy, forKey: Key.cfproxy)
        defaults.set(
            settings.workerDomains.trimmingCharacters(in: .whitespacesAndNewlines),
            forKey: Key.workerDomains
        )
        defaults.set(
            settings.fakeTlsDomain.trimmingCharacters(in: .whitespacesAndNewlines).lowercased(),
            forKey: Key.fakeTls
        )
        defaults.set(
            settings.maskingUpstream.trimmingCharacters(in: .whitespacesAndNewlines).lowercased(),
            forKey: Key.masking
        )
    }

    /// Returns the stored secret, generating and persisting one on first use.
    ///
    /// A Keychain that is merely empty is a first run; a Keychain that refuses
    /// to answer is not. Generating a secret in the second case would silently
    /// invalidate the proxy entry the user already saved in Telegram, so the
    /// failure is propagated instead.
    func secret() throws -> String {
        switch keychain.read() {
        case .found(let stored) where ProxyInputValidator.validSecret(stored):
            return stored
        case .found(_), .missing:
            let generated = Self.generateSecret()
            try keychain.write(generated)
            return generated
        case .failed(let status):
            throw SecretError.unavailable(status)
        }
    }

    func replaceSecret(with secret: String) throws {
        try keychain.write(secret.trimmingCharacters(in: .whitespacesAndNewlines).lowercased())
    }

    func regenerateSecret() throws -> String {
        let generated = Self.generateSecret()
        try keychain.write(generated)
        return generated
    }

    static func generateSecret() -> String {
        var bytes = [UInt8](repeating: 0, count: 16)
        if SecRandomCopyBytes(kSecRandomDefault, bytes.count, &bytes) != errSecSuccess {
            bytes = (0..<16).map { _ in UInt8.random(in: UInt8.min...UInt8.max) }
        }
        return bytes.map { String(format: "%02x", $0) }.joined()
    }

    /// The rotating log file written by the Rust core.
    static var logFileURL: URL {
        let base = FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask)
            .first ?? URL(fileURLWithPath: NSTemporaryDirectory())
        return base.appendingPathComponent("logs/proxy.log")
    }

    /// Serialises the settings into the JSON contract shared with Android.
    func configurationJSON(for settings: ProxySettings) throws -> String {
        let configuration = try ProxyConfiguration(
            port: settings.port,
            secret: secret(),
            poolSize: settings.poolSize,
            fallbackCfproxy: settings.fallbackCfproxy,
            workerDomains: ProxyInputValidator.parseDomains(settings.workerDomains),
            fakeTlsDomain: settings.fakeTlsDomain.trimmingCharacters(
                in: .whitespacesAndNewlines
            ).lowercased(),
            maskingUpstream: settings.maskingUpstream.trimmingCharacters(
                in: .whitespacesAndNewlines
            ).lowercased(),
            logPath: Self.logFileURL.path
        )
        let data = try JSONEncoder().encode(configuration)
        return String(decoding: data, as: UTF8.self)
    }
}

/// Why the secret could not be produced.
enum SecretError: LocalizedError {
    /// The Keychain holds an item but refused to return it, which happens
    /// before the first unlock after a reboot.
    case unavailable(OSStatus)
    case notStored(OSStatus)

    var errorDescription: String? {
        switch self {
        case .unavailable:
            return String(
                localized: "The secret is locked. Unlock the device and try again."
            )
        case .notStored:
            return String(localized: "The secret could not be saved to the Keychain.")
        }
    }
}

private enum KeychainReadResult {
    case found(String)
    case missing
    case failed(OSStatus)
}

/// Minimal Keychain wrapper for a single generic password item.
private struct KeychainSecretStore {
    let service: String
    private let account = "mtproto-secret"

    private var baseQuery: [String: Any] {
        [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service,
            kSecAttrAccount as String: account,
        ]
    }

    func read() -> KeychainReadResult {
        var query = baseQuery
        query[kSecReturnData as String] = true
        query[kSecMatchLimit as String] = kSecMatchLimitOne

        var item: CFTypeRef?
        let status = SecItemCopyMatching(query as CFDictionary, &item)
        switch status {
        case errSecSuccess:
            guard let data = item as? Data, let value = String(data: data, encoding: .utf8) else {
                // The item exists but is not the string we wrote; replacing it
                // is safe because it can never have matched Telegram either.
                return .missing
            }
            return .found(value)
        case errSecItemNotFound:
            return .missing
        default:
            return .failed(status)
        }
    }

    /// Stores the value with `AfterFirstUnlock` accessibility so the proxy can
    /// restart from a Shortcut while the device is locked.
    func write(_ value: String) throws {
        SecItemDelete(baseQuery as CFDictionary)
        var query = baseQuery
        query[kSecValueData as String] = Data(value.utf8)
        query[kSecAttrAccessible as String] = kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly
        let status = SecItemAdd(query as CFDictionary, nil)
        guard status == errSecSuccess else {
            // Returning a secret that was not persisted would rotate it again
            // on the next call, so this has to be reported.
            throw SecretError.notStored(status)
        }
    }
}
