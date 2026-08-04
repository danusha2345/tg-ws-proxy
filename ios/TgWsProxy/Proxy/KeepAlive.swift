import AVFoundation
import UIKit

/// Keeps the process alive after the app leaves the screen.
///
/// iOS has no equivalent of the Android foreground service: once the app is
/// suspended the listener socket on `127.0.0.1` dies and Telegram loses the
/// proxy. The only mechanism that survives without the paid Network Extension
/// entitlement is the `audio` background mode, so the app loops a buffer of
/// digital silence while the proxy runs. The session is created with
/// `.mixWithOthers`, so music from other apps keeps playing.
///
/// The approach is taken from the MIT licensed iOS port by mIwr
/// (https://github.com/mIwr/tg-ws-proxy-ios).
@MainActor
final class KeepAliveController {
    static let shared = KeepAliveController()

    private var player: AVAudioPlayer?
    private var backgroundTask: UIBackgroundTaskIdentifier = .invalid
    private var observersInstalled = false

    var isActive: Bool { player?.isPlaying ?? false }

    private init() {}

    /// Starts looping silence. Returns `false` when the audio session refuses to
    /// activate, in which case the proxy only survives in the foreground.
    @discardableResult
    func start() -> Bool {
        installObservers()
        beginBackgroundTask()
        guard !isActive else { return true }

        let session = AVAudioSession.sharedInstance()
        do {
            try session.setCategory(.playback, mode: .default, options: [.mixWithOthers])
            try session.setActive(true)
        } catch {
            AppLog.shared.append("keep-alive: audio session failed: \(error.localizedDescription)")
            return false
        }

        do {
            let activePlayer: AVAudioPlayer
            if let existing = player {
                activePlayer = existing
            } else {
                activePlayer = try AVAudioPlayer(
                    data: Self.silentWAV(seconds: 5, sampleRate: 8000),
                    fileTypeHint: AVFileType.wav.rawValue
                )
                player = activePlayer
            }
            activePlayer.numberOfLoops = -1
            // The samples are already zero, so the volume only guards against
            // iOS treating a fully muted player as "not playing".
            activePlayer.volume = 0.01
            guard activePlayer.play() else {
                AppLog.shared.append("keep-alive: player refused to start")
                return false
            }
        } catch {
            AppLog.shared.append("keep-alive: player failed: \(error.localizedDescription)")
            return false
        }
        return true
    }

    func stop() {
        player?.stop()
        player = nil
        try? AVAudioSession.sharedInstance().setActive(
            false,
            options: .notifyOthersOnDeactivation
        )
        endBackgroundTask()
    }

    // MARK: - Background task

    /// Bridges the gap between leaving the screen and the audio session taking
    /// over; without it a start issued from a Shortcut can be suspended early.
    private func beginBackgroundTask() {
        guard backgroundTask == .invalid else { return }
        backgroundTask = UIApplication.shared.beginBackgroundTask(withName: "TgWsProxyKeepAlive") {
            [weak self] in
            Task { @MainActor in self?.endBackgroundTask() }
        }
    }

    private func endBackgroundTask() {
        guard backgroundTask != .invalid else { return }
        UIApplication.shared.endBackgroundTask(backgroundTask)
        backgroundTask = .invalid
    }

    // MARK: - Session recovery

    private func installObservers() {
        guard !observersInstalled else { return }
        observersInstalled = true
        let center = NotificationCenter.default
        center.addObserver(
            self,
            selector: #selector(handleInterruption),
            name: AVAudioSession.interruptionNotification,
            object: nil
        )
        center.addObserver(
            self,
            selector: #selector(handleRouteChange),
            name: AVAudioSession.routeChangeNotification,
            object: nil
        )
        center.addObserver(
            self,
            selector: #selector(handleMediaServicesReset),
            name: AVAudioSession.mediaServicesWereResetNotification,
            object: nil
        )
    }

    /// A phone call or another exclusive session stops playback; restart it once
    /// the interruption ends, but only while the proxy still needs us.
    @objc private func handleInterruption(_ notification: Notification) {
        guard
            let raw = notification.userInfo?[AVAudioSessionInterruptionTypeKey] as? UInt,
            let type = AVAudioSession.InterruptionType(rawValue: raw)
        else { return }
        guard type == .ended else { return }
        restartIfNeeded()
    }

    /// Unplugging headphones pauses the player on some routes.
    @objc private func handleRouteChange(_ notification: Notification) {
        guard
            let raw = notification.userInfo?[AVAudioSessionRouteChangeReasonKey] as? UInt,
            let reason = AVAudioSession.RouteChangeReason(rawValue: raw),
            reason == .oldDeviceUnavailable
        else { return }
        restartIfNeeded()
    }

    /// The media daemon can be restarted by the system, which invalidates the
    /// player object itself.
    @objc private func handleMediaServicesReset() {
        player = nil
        restartIfNeeded()
    }

    private func restartIfNeeded() {
        guard ProxyController.shared.status.isActive, !isActive else { return }
        start()
    }

    /// Builds a mono 16-bit PCM WAV of pure silence in memory, so the app does
    /// not have to ship an audio asset.
    private static func silentWAV(seconds: Int, sampleRate: Int) -> Data {
        let channels = 1
        let bitsPerSample = 16
        let blockAlign = channels * bitsPerSample / 8
        let byteRate = sampleRate * blockAlign
        let dataSize = sampleRate * seconds * blockAlign

        var data = Data()
        func append<T: FixedWidthInteger>(_ value: T) {
            withUnsafeBytes(of: value.littleEndian) { data.append(contentsOf: $0) }
        }

        data.append(contentsOf: Array("RIFF".utf8))
        append(UInt32(36 + dataSize))
        data.append(contentsOf: Array("WAVE".utf8))

        data.append(contentsOf: Array("fmt ".utf8))
        append(UInt32(16))
        append(UInt16(1))  // PCM
        append(UInt16(channels))
        append(UInt32(sampleRate))
        append(UInt32(byteRate))
        append(UInt16(blockAlign))
        append(UInt16(bitsPerSample))

        data.append(contentsOf: Array("data".utf8))
        append(UInt32(dataSize))
        data.append(Data(count: dataSize))
        return data
    }
}
