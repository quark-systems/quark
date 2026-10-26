import AppKit

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
}
