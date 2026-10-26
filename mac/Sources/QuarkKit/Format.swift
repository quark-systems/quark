import Foundation

/// Words and numbers the menubar shows, kept here so they are tested.
public enum Format {
    /// "resets in 2h 14m", "resets in 3d 4h", "resets now".
    public static func resets(_ at: Date, now: Date = Date()) -> String {
        let s = Int(at.timeIntervalSince(now))
        if s <= 0 { return "resets now" }
        let d = s / 86_400, h = (s % 86_400) / 3600, m = (s % 3600) / 60
        if d > 0 { return "resets in \(d)d \(h)h" }
        if h > 0 { return "resets in \(h)h \(m)m" }
        return "resets in \(max(m, 1))m"
    }

    /// "42%" for a percentage from 0 to 100.
    public static func percent(_ p: Double) -> String {
        "\(Int(p.rounded()))%"
    }

    /// "12.3 GB" in base-10 units, as Finder shows sizes.
    public static func bytes(_ b: UInt64) -> String {
        ByteCountFormatter.string(fromByteCount: Int64(clamping: b), countStyle: .file)
    }

    /// "checked 3m ago".
    public static func checked(_ at: Date, now: Date = Date()) -> String {
        let s = max(0, Int(now.timeIntervalSince(at)))
        if s < 60 { return "checked just now" }
        if s < 3600 { return "checked \(s / 60)m ago" }
        if s < 86_400 { return "checked \(s / 3600)h ago" }
        return "checked \(s / 86_400)d ago"
    }
}

extension Account {
    /// The percentage to show for the account, when its quota is known.
    public var shownRemaining: Double? {
        quota.state == .known ? quota.remainingPercent : nil
    }

    /// Why there are no numbers, in a few words.
    public var quotaNote: String? {
        switch quota.state {
        case .known: return nil
        case .pending: return "Not read yet"
        case .unsupported: return "Quota not reported"
        case .unavailable, .error, .unknown: return quota.detail ?? "Quota could not be read"
        }
    }
}

/// The lowest known remaining quota across accounts, for the menubar icon.
public func lowestRemaining(_ accounts: [Account]) -> Double? {
    accounts.compactMap(\.shownRemaining).min()
}
