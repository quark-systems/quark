import AppKit
import ServiceManagement

/// Runs the quarkd bundled in QuarkBar.app as a per-user LaunchAgent, so
/// launchd keeps it alive whether or not the menubar app is open, and owns
/// QuarkBar's own launch at login. Starting a process is all it does: quarkd
/// itself is configured through its API.
@MainActor
final class Daemon: ObservableObject {
    static let label = "systems.quark.quarkd"

    private let agent = SMAppService.agent(plistName: "\(Daemon.label).plist")
    private let store: Store
    @Published private(set) var lastError: String?
    /// Bumped after each change so menus read the statuses again.
    @Published private(set) var revision = 0

    init(store: Store) {
        self.store = store
    }

    enum State: Equatable {
        /// Not running from QuarkBar.app (e.g. `swift run`): nothing to manage.
        case notBundled
        /// The LaunchAgent is on: launchd runs quarkd and restarts it.
        case managed
        /// Turned on, waiting for you in System Settings › Login Items.
        case needsApproval
        /// The LaunchAgent is off.
        case off
    }

    var state: State {
        switch agent.status {
        case .enabled: return .managed
        case .requiresApproval: return .needsApproval
        case .notRegistered: return .off
        // macOS can report an agent it has never registered as not found, so
        // "not found" only means not bundled when the plist really is missing.
        case .notFound: return plistBundled ? .off : .notBundled
        @unknown default: return plistBundled ? .off : .notBundled
        }
    }

    private var plistBundled: Bool {
        let plist = Bundle.main.bundleURL
            .appendingPathComponent("Contents/Library/LaunchAgents/\(Daemon.label).plist")
        return FileManager.default.fileExists(atPath: plist.path)
    }

    /// The first time QuarkBar.app runs, it starts quarkd unless one is
    /// already answering (started some other way).
    func startIfFirstRun() {
        let key = "quarkdSetUp"
        guard state == .off, !UserDefaults.standard.bool(forKey: key) else { return }
        Task {
            await store.load()
            UserDefaults.standard.set(true, forKey: key)
            if !store.isConnected { start() }
        }
    }

    func start() {
        do {
            try agent.register()
            lastError = nil
        } catch {
            lastError = error.localizedDescription
        }
        changed()
    }

    func stop() {
        Task {
            do {
                try await agent.unregister()
                lastError = nil
            } catch {
                lastError = error.localizedDescription
            }
            changed()
        }
    }

    /// Restarts the running quarkd; launchd starts it again at once.
    func restart() {
        let p = Process()
        p.executableURL = URL(fileURLWithPath: "/bin/launchctl")
        p.arguments = ["kickstart", "-k", "gui/\(getuid())/\(Daemon.label)"]
        do {
            try p.run()
            p.waitUntilExit()
            lastError = p.terminationStatus == 0 ? nil : "launchctl kickstart exited \(p.terminationStatus)"
        } catch {
            lastError = error.localizedDescription
        }
        changed()
    }

    /// Where the launch script sends quarkd's output.
    static var logURL: URL {
        FileManager.default.homeDirectoryForCurrentUser
            .appendingPathComponent("Library/Logs/Quark/quarkd.log")
    }

    func openLoginItems() {
        SMAppService.openSystemSettingsLoginItems()
    }

    // MARK: QuarkBar at login

    var launchesAtLogin: Bool {
        SMAppService.mainApp.status == .enabled
    }

    func setLaunchesAtLogin(_ on: Bool) {
        do {
            if on { try SMAppService.mainApp.register() } else { try SMAppService.mainApp.unregister() }
            lastError = nil
        } catch {
            lastError = error.localizedDescription
        }
        changed()
    }

    private func changed() {
        revision += 1
        store.loadSoon()
        // quarkd takes a moment to listen after launchd starts it.
        Task {
            try? await Task.sleep(nanoseconds: 2_000_000_000)
            await store.load()
        }
    }
}
