import Foundation

/// Response returned by `tgws_start` and `tgws_stop`.
struct NativeResponse: Decodable {
    let ok: Bool
    let error: String?
}

/// Runtime state and traffic counters reported by the Rust core.
struct ProxyStatus: Equatable {
    var state: String = "stopped"
    var error: String?
    var telegramUrl: String?
    var startedAtEpochSeconds: UInt64?
    var totalConnections: UInt64 = 0
    var activeConnections: UInt64 = 0
    var websocketConnections: UInt64 = 0
    var tcpFallbackConnections: UInt64 = 0
    var cloudflareConnections: UInt64 = 0
    var badConnections: UInt64 = 0
    var bytesUp: UInt64 = 0
    var bytesDown: UInt64 = 0

    /// True while the core owns the listener socket, including the transitions.
    var isActive: Bool {
        state == "starting" || state == "running" || state == "stopping"
    }

    var isRunning: Bool { state == "running" }
}

extension ProxyStatus: Decodable {
    private enum CodingKeys: String, CodingKey {
        case state, error, telegramUrl, startedAtEpochSeconds
        case totalConnections, activeConnections, websocketConnections
        case tcpFallbackConnections, cloudflareConnections, badConnections
        case bytesUp, bytesDown
    }

    /// Decodes defensively: a core that fails while serialising its status
    /// sends `state` and `error` only.
    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        func counter(_ key: CodingKeys) throws -> UInt64 {
            try container.decodeIfPresent(UInt64.self, forKey: key) ?? 0
        }
        state = try container.decodeIfPresent(String.self, forKey: .state) ?? "failed"
        error = try container.decodeIfPresent(String.self, forKey: .error)
        telegramUrl = try container.decodeIfPresent(String.self, forKey: .telegramUrl)
        startedAtEpochSeconds = try container.decodeIfPresent(
            UInt64.self,
            forKey: .startedAtEpochSeconds
        )
        totalConnections = try counter(.totalConnections)
        activeConnections = try counter(.activeConnections)
        websocketConnections = try counter(.websocketConnections)
        tcpFallbackConnections = try counter(.tcpFallbackConnections)
        cloudflareConnections = try counter(.cloudflareConnections)
        badConnections = try counter(.badConnections)
        bytesUp = try counter(.bytesUp)
        bytesDown = try counter(.bytesDown)
    }
}

/// Thin wrapper over the C ABI of `crates/ios-bridge`.
enum NativeBridge {
    static func start(configurationJSON: String) -> NativeResponse {
        let raw = configurationJSON.withCString { pointer in
            takeString(tgws_start(pointer))
        }
        return decode(raw)
    }

    static func stop() -> NativeResponse {
        decode(takeString(tgws_stop()))
    }

    static func status() -> ProxyStatus {
        let raw = takeString(tgws_status())
        guard
            let data = raw.data(using: .utf8),
            let status = try? JSONDecoder().decode(ProxyStatus.self, from: data)
        else {
            var status = ProxyStatus()
            status.state = "failed"
            status.error = String(localized: "The core returned an unreadable status.")
            return status
        }
        return status
    }

    /// Copies a string returned by the core and releases the native allocation.
    private static func takeString(_ pointer: UnsafeMutablePointer<CChar>?) -> String {
        guard let pointer else { return "" }
        defer { tgws_free_string(pointer) }
        return String(cString: pointer)
    }

    private static func decode(_ raw: String) -> NativeResponse {
        guard
            let data = raw.data(using: .utf8),
            let response = try? JSONDecoder().decode(NativeResponse.self, from: data)
        else {
            return NativeResponse(
                ok: false,
                error: String(localized: "The core returned an unreadable response.")
            )
        }
        return response
    }
}
