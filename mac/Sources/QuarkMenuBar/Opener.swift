import AppKit
import QuarkKit

/// Opens places in the Quark desktop app.
///
/// A running desktop app gets the link on its socket (app/src-tauri/src/lib.rs), which
/// works for the bundled app and for one run with `npm run tauri dev`, whose `quark://`
/// links macOS never delivers. Otherwise the link goes through macOS to the bundled app,
/// launching it.
enum Opener {
    static let desktopBundleID = "systems.quark.desktop"
    /// The desktop app's executable, which a dev build runs without a bundle.
    static let desktopExecutable = "quark-desktop"

    static var socketPath: String {
        FileManager.default.homeDirectoryForCurrentUser
            .appendingPathComponent("Library/Application Support/Quark/desktop.sock").path
    }

    static func open(_ link: DeskLink) {
        open(link.url)
    }

    static func open(_ url: URL) {
        let ws = NSWorkspace.shared
        if let running = runningDesktop(), send(url) {
            running.activate()
        } else if ws.urlForApplication(toOpen: url) != nil {
            ws.open(url)
        } else if let app = ws.urlForApplication(withBundleIdentifier: desktopBundleID) {
            // A desktop app without `quark://` links still opens, on its home.
            ws.openApplication(at: app, configuration: NSWorkspace.OpenConfiguration())
        } else {
            NSSound.beep()
        }
    }

    /// Whether there is a desktop app to open: running, or installed.
    static var desktopAvailable: Bool {
        runningDesktop() != nil
            || NSWorkspace.shared.urlForApplication(withBundleIdentifier: desktopBundleID) != nil
    }

    static func runningDesktop() -> NSRunningApplication? {
        NSWorkspace.shared.runningApplications.first {
            $0.bundleIdentifier == desktopBundleID
                || $0.executableURL?.lastPathComponent == desktopExecutable
        }
    }

    /// Writes `url` to the desktop app's socket; false when nothing listens there.
    static func send(_ url: URL) -> Bool {
        let fd = socket(AF_UNIX, SOCK_STREAM, 0)
        guard fd >= 0 else { return false }
        defer { close(fd) }
        var addr = sockaddr_un()
        addr.sun_family = sa_family_t(AF_UNIX)
        let path = Array(socketPath.utf8CString)
        guard path.count <= MemoryLayout.size(ofValue: addr.sun_path) else { return false }
        withUnsafeMutableBytes(of: &addr.sun_path) { raw in
            path.withUnsafeBytes { raw.copyMemory(from: $0) }
        }
        let connected = withUnsafePointer(to: &addr) {
            $0.withMemoryRebound(to: sockaddr.self, capacity: 1) {
                connect(fd, $0, socklen_t(MemoryLayout<sockaddr_un>.size))
            }
        }
        guard connected == 0 else { return false }
        let line = Array((url.absoluteString + "\n").utf8)
        return line.withUnsafeBytes { write(fd, $0.baseAddress, $0.count) } == line.count
    }
}
