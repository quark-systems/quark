import Foundation

// Mirrors of the quarkd API types the menubar reads (crates/quark-systems/src).
// Only the fields the menubar shows are decoded; quarkd may send more.
// Enums decode values this build does not know as `.unknown`, so a newer quarkd
// never breaks an older menubar.

/// `GET /v1/health`.
public struct Health: Codable, Equatable, Sendable {
    public var status: String
    public var version: String
    public var engine: String
    public var lastSeq: Int64
}

// MARK: Status

/// `GET /v1/status`: what needs you and how every Project and its agents are doing.
public struct StatusView: Codable, Equatable, Sendable {
    /// Everything that waits on you, oldest first.
    public var attention: [AttentionItem]
    public var counts: ToneCounts
    public var projects: [ProjectSummary]
}

public struct AttentionItem: Codable, Equatable, Identifiable, Sendable {
    public var key: String
    public var kind: AttentionKind
    /// The decision's, pull request's or worker's id.
    public var targetId: String
    public var title: String
    public var projectId: String
    public var projectName: String
    /// RFC 3339; empty when quarkd does not know.
    public var since: String
    public var id: String { key }
    public var sinceDate: Date? { RFC3339.parse(since) }
}

public enum AttentionKind: String, Codable, Sendable, UnknownDecodable {
    case decision
    case pullRequest = "pull_request"
    case worker
    case unknown
}

/// The glossary's state words.
public enum Tone: String, Codable, Sendable, UnknownDecodable {
    case busy
    case needsYou = "needs_you"
    case ready, failed, parked, idle
    case unknown
}

public struct ToneCounts: Codable, Equatable, Sendable {
    public var busy: UInt32
    public var needsYou: UInt32
    public var ready: UInt32
    public var failed: UInt32
    public var parked: UInt32
    public var idle: UInt32
}

public struct ProjectSummary: Codable, Equatable, Identifiable, Sendable {
    public var id: String
    public var name: String
    /// `provisioning`, `ready` or `failed`.
    public var status: String
    public var statusDetail: String?
    public var attention: UInt32
    public var openDecisions: UInt32
    public var openPullRequests: UInt32
    public var failingPullRequests: UInt32
    public var counts: ToneCounts
    public var workers: [WorkerSummary]
    public var lastActivity: Date?
}

public struct WorkerSummary: Codable, Equatable, Identifiable, Sendable {
    public var id: String
    public var title: String
    public var state: String
    public var tone: Tone
    public var note: String?
    public var harness: String?
    public var pullRequestUrl: String?
    public var updatedAt: Date?
}

/// `notice.posted` payload: an update the away policy decided to tell you about.
public struct Notice: Codable, Equatable, Sendable {
    public var projectId: String
    /// `now` (one update), `digest` or `return`.
    public var kind: String
    public var text: String
    public var taskId: String?
    public var occasion: String?
}

// MARK: Accounts

/// `GET /v1/accounts`: one account of a harness, with its latest quota reading.
public struct Account: Codable, Equatable, Identifiable, Sendable {
    public var id: String
    public var harness: String
    public var label: String
    public var `default`: Bool
    public var health: HarnessAuth
    public var quota: AccountQuota
    public var activeTasks: UInt32
}

public struct HarnessAuth: Codable, Equatable, Sendable {
    public var state: AuthState
    public var detail: String
}

public enum AuthState: String, Codable, Sendable, UnknownDecodable {
    case configured
    case notConfigured = "not_configured"
    case unknown
}

public struct AccountQuota: Codable, Equatable, Sendable {
    public var state: QuotaState
    /// What limits the account now, 0 to 100.
    public var remainingPercent: Double?
    public var plan: String?
    public var windows: [QuotaWindow]
    public var detail: String?
    public var checkedAt: Date?
}

public enum QuotaState: String, Codable, Sendable, UnknownDecodable {
    case pending, known, unavailable, error, unsupported, unknown
}

/// One rate-limit window of an account's plan, e.g. the 5-hour session or the week.
public struct QuotaWindow: Codable, Equatable, Identifiable, Sendable {
    public var id: String
    public var label: String
    public var percentRemaining: Double?
    public var resetsAt: Date?
}

/// `GET /v1/harnesses`: the menubar only needs each harness's display name.
public struct Harness: Codable, Equatable, Identifiable, Sendable {
    public var id: String
    public var name: String
}

// MARK: Hosts

/// `GET /v1/hosts`.
public struct HostsView: Codable, Equatable, Sendable {
    public var hours: UInt32
    public var hosts: [Host]
    public var error: String?
}

public struct Host: Codable, Equatable, Identifiable, Sendable {
    public var id: String
    public var name: String
    public var runtime: HostRuntime
    public var os: String
    public var arch: String
    public var capacity: HostCapacity
    public var health: HostHealth
    public var latest: HostReading?
    public var projects: [ProjectUsage]
    public var worktrees: HostWorktrees?
}

public enum HostRuntime: String, Codable, Sendable, UnknownDecodable {
    case local, ssh, hosted
    case privateCloud = "private_cloud"
    case unknown
}

public struct HostCapacity: Codable, Equatable, Sendable {
    public var cpus: UInt32
    public var memoryBytes: UInt64
    public var diskBytes: UInt64
    public var maxWorkers: UInt32
}

public struct HostHealth: Codable, Equatable, Sendable {
    public var status: HostHealthStatus
    public var reason: String?
    public var since: Date?
}

public enum HostHealthStatus: String, Codable, Sendable, UnknownDecodable {
    case healthy, degraded, unreachable, unknown
}

public struct HostReading: Codable, Equatable, Sendable {
    public var at: Date
    /// 0 to 1 across all cores.
    public var cpu: Double
    public var memoryUsedBytes: UInt64
    public var memoryTotalBytes: UInt64
    /// 0 (none) to 1 (critical).
    public var memoryPressure: Double
    public var diskFreeBytes: UInt64
}

public struct ProjectUsage: Codable, Equatable, Sendable {
    public var projectId: String
    public var projectName: String?
    public var cpu: Double
    public var memoryBytes: UInt64
}

public struct HostWorktrees: Codable, Equatable, Sendable {
    public var idle: UInt32
    public var inUse: UInt32
    public var dirty: UInt32
    public var leased: UInt32
    public var quarantined: UInt32
    public var error: String?
}

// MARK: Events

/// One frame of `GET /v1/events` (WebSocket). Only `notice.posted` payloads
/// are decoded; for everything else the type alone says what to read again.
public struct Event: Decodable, Equatable, Sendable {
    public var seq: Int64
    public var projectId: String?
    public var type: String
    public var ts: String
    /// The update, for `notice.posted`.
    public var notice: Notice?

    enum CodingKeys: String, CodingKey { case seq, projectId, type, ts, payload }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        seq = try c.decode(Int64.self, forKey: .seq)
        projectId = try c.decodeIfPresent(String.self, forKey: .projectId)
        type = try c.decode(String.self, forKey: .type)
        ts = try c.decode(String.self, forKey: .ts)
        notice = type == "notice.posted" ? try? c.decode(Notice.self, forKey: .payload) : nil
    }
}

// MARK: Decoding

/// A string enum whose `unknown` case absorbs values this build does not know.
public protocol UnknownDecodable: RawRepresentable<String>, Decodable {
    static var unknown: Self { get }
}

extension UnknownDecodable {
    public init(from decoder: Decoder) throws {
        let raw = try decoder.singleValueContainer().decode(String.self)
        self = Self(rawValue: raw) ?? .unknown
    }
}

extension JSONDecoder {
    /// Decodes quarkd's JSON: snake_case keys and RFC 3339 UTC timestamps,
    /// with or without fractional seconds.
    public static var quark: JSONDecoder {
        let d = JSONDecoder()
        d.keyDecodingStrategy = .convertFromSnakeCase
        d.dateDecodingStrategy = .custom { decoder in
            let s = try decoder.singleValueContainer().decode(String.self)
            if let date = RFC3339.parse(s) { return date }
            throw DecodingError.dataCorrupted(.init(
                codingPath: decoder.codingPath,
                debugDescription: "not an RFC 3339 timestamp: \(s)"))
        }
        return d
    }
}

enum RFC3339 {
    static func parse(_ s: String) -> Date? {
        let plain = ISO8601DateFormatter()
        plain.formatOptions = [.withInternetDateTime]
        if let d = plain.date(from: s) { return d }
        let fractional = ISO8601DateFormatter()
        fractional.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        return fractional.date(from: s)
    }
}
