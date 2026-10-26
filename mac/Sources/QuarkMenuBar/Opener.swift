import AppKit
import QuarkKit

/// Opens places in the Quark desktop app.
enum Opener {
    static let desktopBundleID = "systems.quark.desktop"

    static func open(_ link: DeskLink) {
        open(link.url)
    }

    static func open(_ url: URL) {
        let ws = NSWorkspace.shared
        if ws.urlForApplication(toOpen: url) != nil {
            ws.open(url)
        } else if let app = ws.urlForApplication(withBundleIdentifier: desktopBundleID) {
            // A desktop app without `quark://` links still opens, on its home.
            ws.openApplication(at: app, configuration: NSWorkspace.OpenConfiguration())
        }
    }

    static var desktopInstalled: Bool {
        NSWorkspace.shared.urlForApplication(withBundleIdentifier: desktopBundleID) != nil
    }
}
