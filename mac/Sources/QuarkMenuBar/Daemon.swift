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

    /// The build of QuarkBar that last turned the LaunchAgent on, so that
    /// afterwards an agent that is off was turned off on purpose (Stop quarkd)
    /// and stays off, until another build runs. The defaults are shared by
    /// every QuarkBar.app (a build from source in mac/dist, an installed
    /// Quark), so a plain flag set by one left a freshly installed one never
    /// starting quarkd. Not the old "quarkdSetUp" key, which was also set when
    /// another quarkd was answering.
    private static let agentKey = "quarkdAgentOnFor"
    /// The same for opening QuarkBar at login, which is per app bundle too.
    private static let loginKey = "loginItemSetUpFor"

    /// This QuarkBar.app: where it is, its version and its build number.
    static var buildIdentity: String {
        let info = Bundle.main.infoDictionary ?? [:]
        let version = info["CFBundleShortVersionString"] as? String ?? "?"
        let build = info["CFBundleVersion"] as? String ?? "?"
        return "\(Bundle.main.bundleURL.path)#\(version)(\(build))"
    }

    private static func doneByThisBuild(_ key: String) -> Bool {
        UserDefaults.standard.string(forKey: key) == buildIdentity
    }

    private static func markDoneByThisBuild(_ key: String) {
        UserDefaults.standard.set(buildIdentity, forKey: key)
    }

    /// The first time each build of QuarkBar.app runs, it opens itself at
    /// login and turns quarkd's LaunchAgent on, unless a quarkd is already
    /// answering (started some other way, e.g. from source); then it looks
    /// again at its next launch, which is the next login at the latest.
    /// Quark.app opens the QuarkBar inside it on every launch, so installing
    /// Quark and opening it once sets up all three.
    func startIfFirstRun() {
        if state != .notBundled, !Daemon.doneByThisBuild(Daemon.loginKey) {
            Daemon.markDoneByThisBuild(Daemon.loginKey)
            if !launchesAtLogin { setLaunchesAtLogin(true) }
        }
        if state == .managed || state == .needsApproval {
            Daemon.markDoneByThisBuild(Daemon.agentKey)
        }
        guard state == .off, !Daemon.doneByThisBuild(Daemon.agentKey) else { return }
        Task {
            await store.load()
            if !store.isConnected { start() }
        }
    }

    func start() {
        do {
            try agent.register()
            Daemon.markDoneByThisBuild(Daemon.agentKey)
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
