import Combine
import Foundation
import QuarkKit

/// What quarkd last told the menubar. Read on a timer and again whenever an
/// event says something changed; nothing here is computed from anything but
/// quarkd's answers.
@MainActor
final class Store: ObservableObject {
    enum Connection: Equatable {
        case connecting
        case connected(Health)
        case unreachable(String)
    }

    @Published private(set) var connection: Connection = .connecting
    @Published private(set) var status: StatusView?
    @Published private(set) var accounts: [Account] = []
    @Published private(set) var hosts: HostsView?
    @Published private(set) var harnessNames: [String: String] = [:]
    @Published private(set) var refreshingQuota = false
    @Published private(set) var now = Date()

    /// Called with each update the away policy decided to tell you about.
    var onNotice: ((Notice) -> Void)?

    let client: QuarkClient
    private var poll: Task<Void, Never>?
    private var events: Task<Void, Never>?
    private var pending: Task<Void, Never>?

    /// Event types after which the status, accounts or hosts are read again.
    private static let reloadOn = ["task.", "decision.", "pr.", "check.", "project.", "account.", "host."]

    init(client: QuarkClient = QuarkClient()) {
        self.client = client
    }

    var isConnected: Bool {
        if case .connected = connection { return true }
        return false
    }

    var needsYou: Int { status?.attention.count ?? 0 }

    func start() {
        poll = Task { [weak self] in
            while !Task.isCancelled {
                await self?.load()
                try? await Task.sleep(nanoseconds: 60 * 1_000_000_000)
            }
        }
        events = Task { [weak self, client] in
            for await event in quarkEvents(client) {
                guard let self else { return }
                if let notice = event.notice { self.onNotice?(notice) }
                if Self.reloadOn.contains(where: event.type.hasPrefix) { self.loadSoon() }
            }
        }
    }

    /// Reads everything now.
    func load() async {
        now = Date()
        do {
            connection = .connected(try await client.health())
        } catch {
            connection = .unreachable(error.localizedDescription)
            return
        }
        async let status = client.status()
        async let accounts = client.accounts()
        async let hosts = client.hosts()
        async let harnesses = client.harnesses()
        if let s = try? await status { self.status = s }
        if let a = try? await accounts { self.accounts = a }
        if let h = try? await hosts { self.hosts = h }
        if let hs = try? await harnesses, !hs.isEmpty {
            harnessNames = Dictionary(hs.map { ($0.id, $0.name) }, uniquingKeysWith: { a, _ in a })
        }
    }

    /// Reads again shortly, folding a burst of events into one read.
    func loadSoon() {
        pending?.cancel()
        pending = Task { [weak self] in
            try? await Task.sleep(nanoseconds: 500_000_000)
            guard !Task.isCancelled else { return }
            await self?.load()
        }
    }

    /// Asks quarkd to read every account's quota now.
    func refreshQuota() {
        guard !refreshingQuota else { return }
        refreshingQuota = true
        Task {
            if let a = try? await client.accounts(refresh: true) { accounts = a }
            refreshingQuota = false
            now = Date()
        }
    }

    func harnessName(_ id: String) -> String {
        harnessNames[id] ?? id
    }

    func projectName(_ id: String) -> String? {
        status?.projects.first { $0.id == id }?.name
    }
}
