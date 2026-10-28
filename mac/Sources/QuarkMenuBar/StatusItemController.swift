import AppKit
import Combine
import QuarkKit
import QuarkUI

/// The menubar icon and its menu. The icon carries a count of what needs you;
/// the menu leads with that, then each Project and its agents, then hosts and
/// quota. Rebuilt whenever quarkd's answers change, including while open.
@MainActor
final class StatusItemController: NSObject, NSMenuDelegate {
    private let store: Store
    private let daemon: Daemon
    private let item = NSStatusBar.system.statusItem(withLength: NSStatusItem.variableLength)
    private let menu = NSMenu()
    private var isOpen = false
    private var bag = Set<AnyCancellable>()

    /// Most attention items listed before "and N more".
    private let attentionShown = 8

    init(store: Store, daemon: Daemon) {
        self.store = store
        self.daemon = daemon
        super.init()
        menu.delegate = self
        menu.autoenablesItems = false
        item.menu = menu
        item.button?.imagePosition = .imageLeading
        updateIcon()
        store.objectWillChange.merge(with: daemon.objectWillChange)
            .debounce(for: .milliseconds(100), scheduler: RunLoop.main)
            .sink { [weak self] in
                self?.updateIcon()
                if self?.isOpen == true { self?.rebuild() }
            }
            .store(in: &bag)
    }

    // MARK: Icon

    private func updateIcon() {
        guard let button = item.button else { return }
        let image = NSImage(systemSymbolName: "atom", accessibilityDescription: "Quark")
        image?.isTemplate = true
        button.image = image
        button.appearsDisabled = !store.isConnected
        let n = store.needsYou
        if store.isConnected, n > 0 {
            button.attributedTitle = NSAttributedString(string: " \(n)", attributes: [
                .font: NSFont.monospacedDigitSystemFont(ofSize: 12, weight: .semibold),
                .foregroundColor: NSColor.systemPurple,
            ])
            button.toolTip = "\(n) waiting on you"
        } else {
            button.attributedTitle = NSAttributedString(string: "")
            button.toolTip = store.isConnected ? "Nothing needs you" : "quarkd isn't running"
        }
    }

    // MARK: Menu

    func menuWillOpen(_ menu: NSMenu) {
        isOpen = true
        store.loadSoon()
    }

    func menuDidClose(_ menu: NSMenu) {
        isOpen = false
    }

    func menuNeedsUpdate(_ menu: NSMenu) {
        rebuild()
    }

    private func rebuild() {
        menu.removeAllItems()
        menu.addItem(hostedItem(HeaderView(store: store)))

        if store.isConnected, let status = store.status {
            addAttention(status)
            addProjects(status)
            addHosts()
            addQuota()
        }
        menu.addItem(.separator())
        addActions()
    }

    private func addAttention(_ status: StatusView) {
        guard !status.attention.isEmpty else { return }
        menu.addItem(.separator())
        menu.addItem(.sectionHeader(title: "Needs you"))
        for a in status.attention.prefix(attentionShown) {
            let kind = a.kind.label
            let detail = [a.projectName, kind, a.sinceDate.map { ago($0) }].compactMap { $0 }.joined(separator: " · ")
            menu.addItem(action(twoLine(a.title, detail, dot: .systemPurple)) { Opener.open(DeskLink(a)) })
        }
        let more = status.attention.count - attentionShown
        if more > 0 {
            menu.addItem(action(NSAttributedString(string: "and \(more) more…")) { Opener.open(.home) })
        }
    }

    private func addProjects(_ status: StatusView) {
        menu.addItem(.separator())
        menu.addItem(.sectionHeader(title: "Projects"))
        if status.projects.isEmpty {
            let mi = NSMenuItem(title: "No projects yet", action: nil, keyEquivalent: "")
            mi.isEnabled = false
            menu.addItem(mi)
            return
        }
        for p in status.projects {
            let mi = action(twoLine(p.name, p.line(), dot: p.tone.nsColor)) { Opener.open(.project(p.id)) }
            mi.submenu = projectMenu(p)
            menu.addItem(mi)
        }
    }

    private func projectMenu(_ p: ProjectSummary) -> NSMenu {
        let sub = NSMenu()
        sub.autoenablesItems = false
        sub.addItem(action(NSAttributedString(string: "Open \(p.name)")) { Opener.open(.project(p.id)) })
        if p.openDecisions > 0 || p.openPullRequests > 0 {
            let line = [
                p.openDecisions > 0 ? "\(p.openDecisions) decision\(p.openDecisions == 1 ? "" : "s") open" : nil,
                p.openPullRequests > 0 ? "\(p.openPullRequests) PR\(p.openPullRequests == 1 ? "" : "s") open" : nil,
            ].compactMap { $0 }.joined(separator: " · ")
            let info = NSMenuItem(title: line, action: nil, keyEquivalent: "")
            info.isEnabled = false
            sub.addItem(info)
        }
        sub.addItem(.separator())
        sub.addItem(.sectionHeader(title: p.workers.isEmpty ? "No workers" : "Workers"))
        for w in p.workers {
            let detail = [w.tone.label, w.note, w.harness.map(store.harnessName)].compactMap { $0 }.joined(separator: " · ")
            sub.addItem(action(twoLine(w.title, detail, dot: w.tone.nsColor)) { Opener.open(.worker(w.id)) })
        }
        return sub
    }

    private func addHosts() {
        guard let hosts = store.hosts?.hosts, !hosts.isEmpty else { return }
        menu.addItem(.separator())
        menu.addItem(.sectionHeader(title: "Hosts"))
        for h in hosts {
            menu.addItem(hostedItem(HostRow(host: h)))
        }
    }

    private func addQuota() {
        let accounts = store.accounts.filter { $0.quota.state != .unsupported }
        guard !accounts.isEmpty else { return }
        menu.addItem(.separator())
        menu.addItem(.sectionHeader(title: "Quota"))
        for a in accounts {
            menu.addItem(hostedItem(QuotaRow(account: a, harness: store.harnessName(a.harness), now: store.now)))
        }
    }

    private func addActions() {
        let open = action(NSAttributedString(string: "Open Quark"), key: "o") { Opener.open(.home) }
        open.isEnabled = Opener.desktopAvailable
        menu.addItem(open)
        let refresh = action(NSAttributedString(string: store.refreshingQuota ? "Reading quota…" : "Refresh"), key: "r") { [store] in
            store.loadSoon()
            store.refreshQuota()
        }
        refresh.isEnabled = store.isConnected && !store.refreshingQuota
        menu.addItem(refresh)
        menu.addItem(.separator())
        menu.addItem(daemonItem())
        let login = action(NSAttributedString(string: "Open QuarkBar at Login")) { [daemon] in
            daemon.setLaunchesAtLogin(!daemon.launchesAtLogin)
        }
        login.state = daemon.launchesAtLogin ? .on : .off
        login.isEnabled = Bundle.main.bundleIdentifier != nil
        menu.addItem(login)
        menu.addItem(.separator())
        menu.addItem(action(NSAttributedString(string: "Quit QuarkBar"), key: "q") { NSApp.terminate(nil) })
    }

    private func daemonItem() -> NSMenuItem {
        let state = daemon.state
        let running = store.isConnected
        let line: String
        switch (state, running) {
        case (.managed, true): line = "Running, kept alive by QuarkBar"
        case (.managed, false): line = "Starting…"
        case (.needsApproval, _): line = "Waiting for approval in Login Items"
        case (_, true): line = "Running, started outside QuarkBar"
        case (_, false): line = "Not running"
        }
        let top = NSMenuItem()
        top.attributedTitle = twoLine("quarkd", line, dot: running ? .systemGreen : state == .managed ? .systemOrange : .systemRed)
        let sub = NSMenu()
        sub.autoenablesItems = false
        switch state {
        case .off:
            let start = action(NSAttributedString(string: "Start quarkd")) { [daemon] in daemon.start() }
            start.isEnabled = !running
            if running { start.toolTip = "A quarkd started outside QuarkBar is answering; stop it first." }
            sub.addItem(start)
        case .managed:
            sub.addItem(action(NSAttributedString(string: "Restart quarkd")) { [daemon] in daemon.restart() })
            sub.addItem(action(NSAttributedString(string: "Stop quarkd")) { [daemon] in daemon.stop() })
        case .needsApproval:
            sub.addItem(action(NSAttributedString(string: "Open Login Items…")) { [daemon] in daemon.openLoginItems() })
        case .notBundled:
            let mi = NSMenuItem(title: "Run QuarkBar.app to manage quarkd", action: nil, keyEquivalent: "")
            mi.isEnabled = false
            sub.addItem(mi)
        }
        sub.addItem(action(NSAttributedString(string: "Show Log")) {
            NSWorkspace.shared.open(Daemon.logURL)
        })
        if let e = daemon.lastError {
            let err = NSMenuItem(title: e, action: nil, keyEquivalent: "")
            err.isEnabled = false
            sub.addItem(.separator())
            sub.addItem(err)
        }
        top.submenu = sub
        return top
    }

    // MARK: Helpers


    /// A clickable item running `run`.
    private func action(_ title: NSAttributedString, key: String = "", _ run: @escaping () -> Void) -> NSMenuItem {
        let mi = NSMenuItem(title: title.string, action: #selector(fire(_:)), keyEquivalent: key)
        mi.attributedTitle = title
        mi.target = self
        mi.representedObject = Handler(run)
        return mi
    }

    @objc private func fire(_ sender: NSMenuItem) {
        (sender.representedObject as? Handler)?.run()
    }
}

private final class Handler {
    let run: () -> Void
    init(_ run: @escaping () -> Void) { self.run = run }
}
