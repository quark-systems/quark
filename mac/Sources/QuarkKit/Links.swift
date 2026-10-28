import Foundation

/// Places in the Quark desktop app, as `quark://open/<path>` links. The path is
/// the app's own hash route (app/src/nav.ts), so the app only has to set it.
public enum DeskLink {
    case home
    case project(String)
    case worker(String)
    case decision(String)
    case pullRequest(String)
    case accounts
    case hosts

    public var path: String {
        switch self {
        case .home: return ""
        case let .project(id): return "p/\(escape(id))"
        case let .worker(id): return "t/\(escape(id))"
        case let .decision(id): return "inbox/\(escape(id))"
        case let .pullRequest(id): return "pr/\(escape(id))"
        case .accounts: return "accounts"
        case .hosts: return "hosts"
        }
    }

    public var url: URL { URL(string: "quark://open/\(path)")! }

    /// The same place as a `quarkbar://open/<path>` link, which QuarkBar opens. Widgets
    /// use it, so a click reaches a desktop app that macOS cannot hand `quark://` to.
    public var barURL: URL { URL(string: "quarkbar://open/\(path)")! }

    /// The `quark://` link a `quarkbar://` link stands for.
    public static func deskURL(fromBar url: URL) -> URL? {
        guard url.scheme == "quarkbar" else { return nil }
        let s = url.absoluteString
        return URL(string: "quark" + s.dropFirst("quarkbar".count))
    }

    /// Where an attention item is answered.
    public init(_ item: AttentionItem) {
        switch item.kind {
        case .decision: self = .decision(item.targetId)
        case .pullRequest: self = .pullRequest(item.targetId)
        case .worker: self = .worker(item.targetId)
        case .unknown: self = .project(item.projectId)
        }
    }

    /// Where a notice is about: its worker, else its Project.
    public init(_ notice: Notice) {
        self = notice.taskId.map(DeskLink.worker) ?? .project(notice.projectId)
    }
}

private func escape(_ s: String) -> String {
    s.addingPercentEncoding(withAllowedCharacters: .urlPathAllowed.subtracting(CharacterSet(charactersIn: "/"))) ?? s
}

extension Notice {
    /// What happened, in the app's words (app/src/shell/updates.ts).
    public var occasionLabel: String {
        switch occasion {
        case "progress": return "Progress"
        case "paused": return "Waiting"
        case "done": return "Finished"
        case "decision": return "Needs your decision"
        case "blocked": return "Blocked"
        case "failed": return "Failed"
        case "stale": return "Stopped responding"
        case "inbound": return "Message"
        case "trigger_fired": return "Rule ran"
        case "trigger_failed": return "Rule failed"
        default: return kind == "now" ? "Update" : "While you were away"
        }
    }
}
