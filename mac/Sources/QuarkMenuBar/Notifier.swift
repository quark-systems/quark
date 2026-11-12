import AppKit
import QuarkKit
import UserNotifications

/// Shows quarkd's `notice.posted` updates as macOS notifications. quarkd's away
/// policy decides what is worth telling you; this only shows it.
@MainActor
final class Notifier: NSObject, UNUserNotificationCenterDelegate {
    private let store: Store
    /// Notification Center needs a bundled app; `swift run` has none.
    private let available = Bundle.main.bundleIdentifier != nil

    init(store: Store) {
        self.store = store
        super.init()
        guard available else { return }
        let center = UNUserNotificationCenter.current()
        center.delegate = self
        center.requestAuthorization(options: [.alert, .sound, .badge]) { _, _ in }
        store.onNotice = { [weak self] in self?.post($0) }
        store.onUpdateReady = { [weak self] in self?.postUpdate($0) }
    }

    /// An update is downloaded; clicking opens Updates in the app.
    func postUpdate(_ offer: UpdateStatus.Offer) {
        guard available else { return }
        let content = UNMutableNotificationContent()
        content.title = "Quark \(offer.version) is ready"
        content.body = offer.replacesPulled == true
            ? "This version replaces one that was withdrawn. Install it from the QuarkBar menu."
            : "Install and restart from the QuarkBar menu. Running workers carry on."
        content.userInfo = ["link": DeskLink.checkForUpdates.url.absoluteString]
        let request = UNNotificationRequest(identifier: "update-\(offer.version)", content: content, trigger: nil)
        UNUserNotificationCenter.current().add(request)
    }

    func post(_ notice: Notice) {
        guard available else { return }
        let content = UNMutableNotificationContent()
        let project = store.projectName(notice.projectId) ?? "Quark"
        content.title = project
        content.subtitle = notice.occasionLabel
        content.body = notice.text
        content.threadIdentifier = notice.projectId
        content.userInfo = ["link": DeskLink(notice).url.absoluteString]
        if ["decision", "blocked", "failed", "stale"].contains(notice.occasion ?? "") {
            content.sound = .default
        }
        let request = UNNotificationRequest(identifier: UUID().uuidString, content: content, trigger: nil)
        UNUserNotificationCenter.current().add(request)
    }

    nonisolated func userNotificationCenter(
        _ center: UNUserNotificationCenter,
        willPresent notification: UNNotification
    ) async -> UNNotificationPresentationOptions {
        [.banner, .list, .sound]
    }

    nonisolated func userNotificationCenter(
        _ center: UNUserNotificationCenter,
        didReceive response: UNNotificationResponse
    ) async {
        guard let s = response.notification.request.content.userInfo["link"] as? String,
              let url = URL(string: s) else { return }
        await MainActor.run { Opener.open(url) }
    }
}
