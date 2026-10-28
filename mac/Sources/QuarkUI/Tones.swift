import AppKit
import QuarkKit
import SwiftUI

// How Quark's state words look, shared by the menubar and the widgets.

extension Tone {
    public var nsColor: NSColor {
        switch self {
        case .busy: return .systemBlue
        case .needsYou: return .systemPurple
        case .ready: return .systemGreen
        case .failed: return .systemRed
        case .parked: return .systemOrange
        case .idle, .unknown: return .tertiaryLabelColor
        }
    }

    public var color: Color { Color(nsColor: nsColor) }

    public var label: String {
        switch self {
        case .busy: return "Busy"
        case .needsYou: return "Needs you"
        case .ready: return "Ready"
        case .failed: return "Failed"
        case .parked: return "Parked"
        case .idle, .unknown: return "Idle"
        }
    }
}

extension ToneCounts {
    /// "1 needs you · 2 busy · 1 ready", most urgent first; empty when all idle.
    public var summary: String {
        [(needsYou, "needs you", "need you"), (failed, "failed", "failed"), (busy, "busy", "busy"),
         (ready, "ready", "ready"), (parked, "parked", "parked")]
            .filter { $0.0 > 0 }
            .map { "\($0.0) \($0.0 == 1 ? $0.1 : $0.2)" }
            .joined(separator: " · ")
    }

    /// The tone a group of workers shows: its most urgent one.
    public var lead: Tone {
        if needsYou > 0 { return .needsYou }
        if failed > 0 { return .failed }
        if busy > 0 { return .busy }
        if ready > 0 { return .ready }
        if parked > 0 { return .parked }
        return .idle
    }
}

extension AttentionKind {
    public var label: String {
        switch self {
        case .decision: return "Decision"
        case .pullRequest: return "Check failed"
        case .worker: return "Worker stuck"
        case .unknown: return "Waiting"
        }
    }

    public var symbol: String {
        switch self {
        case .decision: return "questionmark.bubble"
        case .pullRequest: return "xmark.octagon"
        case .worker: return "exclamationmark.triangle"
        case .unknown: return "circle"
        }
    }
}

extension ProjectSummary {
    /// The project's second line: setup state, workers by tone, red PRs.
    public func line(now: Date = Date()) -> String {
        var parts: [String] = []
        if status == "provisioning" { parts.append("Setting up") }
        if status == "failed" { parts.append("Setup failed") }
        if !counts.summary.isEmpty { parts.append(counts.summary) }
        if failingPullRequests > 0 { parts.append("\(failingPullRequests) PR\(failingPullRequests == 1 ? "" : "s") red") }
        if parts.isEmpty { parts.append(lastActivity.map { "Quiet · \(ago($0, now: now))" } ?? "Quiet") }
        return parts.joined(separator: " · ")
    }

    public var tone: Tone { status == "failed" ? .failed : counts.lead }
}

private let relative: RelativeDateTimeFormatter = {
    let f = RelativeDateTimeFormatter()
    f.unitsStyle = .abbreviated
    return f
}()

/// "3 min. ago".
public func ago(_ date: Date, now: Date = Date()) -> String {
    relative.localizedString(for: date, relativeTo: now)
}

/// A thin horizontal bar filled to `fraction`.
public struct Meter: View {
    let fraction: Double
    let tint: Color

    public init(fraction: Double, tint: Color) {
        self.fraction = fraction
        self.tint = tint
    }

    public var body: some View {
        GeometryReader { g in
            ZStack(alignment: .leading) {
                Capsule().fill(Color.primary.opacity(0.1))
                Capsule().fill(tint).frame(width: g.size.width * min(max(fraction, 0), 1))
            }
        }
        .frame(height: 5)
    }
}

/// The tint of a quota bar with `percent` left.
public func quotaTint(_ percent: Double) -> Color {
    percent <= 10 ? .red : percent <= 30 ? .orange : .accentColor
}
