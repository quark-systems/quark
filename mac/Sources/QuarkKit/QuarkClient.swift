import Foundation
#if canImport(FoundationNetworking)
import FoundationNetworking
#endif

/// The quarkd HTTP API, as the menubar uses it.
public struct QuarkClient: Sendable {
    /// Where the desktop app also looks for quarkd unless `QUARK_DAEMON` says otherwise.
    public static let defaultBase = URL(string: "http://127.0.0.1:7380")!

    public var base: URL
    var session: URLSession

    public init(base: URL = QuarkClient.configuredBase(), session: URLSession = .shared) {
        self.base = base
        self.session = session
    }

    /// `QUARK_DAEMON` when set, as for the desktop app, else the default address.
    public static func configuredBase(_ env: [String: String] = ProcessInfo.processInfo.environment) -> URL {
        if let d = env["QUARK_DAEMON"], !d.isEmpty, let url = URL(string: d) { return url }
        return defaultBase
    }

    public func health() async throws -> Health {
        try await get("/v1/health")
    }

    /// What needs you and every Project's agents.
    public func status() async throws -> StatusView {
        try await get("/v1/status")
    }

    /// Every account with its latest quota reading. `refresh` reads each
    /// account's quota now instead of returning the last reading.
    public func accounts(refresh: Bool = false) async throws -> [Account] {
        try await get("/v1/accounts", query: refresh ? [URLQueryItem(name: "refresh", value: "true")] : [])
    }

    public func harnesses() async throws -> [Harness] {
        try await get("/v1/harnesses")
    }

    public func hosts(hours: Int = 1) async throws -> HostsView {
        try await get("/v1/hosts", query: [URLQueryItem(name: "hours", value: String(hours))])
    }

    /// The WebSocket URL of the event stream, replaying after `cursor` when given.
    public func eventsURL(cursor: Int64?) -> URL {
        var c = URLComponents(url: base.appendingPathComponent("v1/events"), resolvingAgainstBaseURL: false)!
        c.scheme = base.scheme == "https" ? "wss" : "ws"
        if let cursor { c.queryItems = [URLQueryItem(name: "cursor", value: String(cursor))] }
        return c.url!
    }

    func get<T: Decodable>(_ path: String, query: [URLQueryItem] = []) async throws -> T {
        var c = URLComponents(url: base.appendingPathComponent(String(path.dropFirst())), resolvingAgainstBaseURL: false)!
        if !query.isEmpty { c.queryItems = query }
        var req = URLRequest(url: c.url!)
        req.timeoutInterval = 30
        let (data, resp) = try await session.data(for: req)
        guard let http = resp as? HTTPURLResponse else { throw QuarkError.notHTTP }
        guard (200..<300).contains(http.statusCode) else {
            let body = try? JSONDecoder().decode(ErrorBody.self, from: data)
            throw QuarkError.status(http.statusCode, body?.error.message ?? String(decoding: data, as: UTF8.self))
        }
        return try JSONDecoder.quark.decode(T.self, from: data)
    }
}

struct ErrorBody: Decodable {
    struct Detail: Decodable { var message: String }
    var error: Detail
}

public enum QuarkError: Error, Equatable, LocalizedError {
    case notHTTP
    case status(Int, String)

    public var errorDescription: String? {
        switch self {
        case .notHTTP: return "quarkd sent something other than HTTP"
        case let .status(code, message): return "quarkd answered \(code): \(message)"
        }
    }
}
