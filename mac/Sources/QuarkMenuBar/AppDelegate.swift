import AppKit
import QuarkKit

@MainActor
final class AppDelegate: NSObject, NSApplicationDelegate {
    private var store: Store!
    private var daemon: Daemon!
    private var notifier: Notifier!
    private var statusItem: StatusItemController!

    func applicationDidFinishLaunching(_ notification: Notification) {
        store = Store()
        daemon = Daemon(store: store)
        notifier = Notifier(store: store)
        statusItem = StatusItemController(store: store, daemon: daemon)
        daemon.startIfFirstRun()
        store.start()
    }

    /// `quarkbar://open/<route>` links, from the widgets.
    func application(_ application: NSApplication, open urls: [URL]) {
        for url in urls {
            if let desk = DeskLink.deskURL(fromBar: url) { Opener.open(desk) }
        }
    }
}
