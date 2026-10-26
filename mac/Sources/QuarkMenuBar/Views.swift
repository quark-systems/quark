import AppKit
import QuarkKit
import SwiftUI

/// Width of the rich rows hosted in the menu.
let rowWidth: CGFloat = 320

extension Tone {
    var color: NSColor {
        switch self {
        case .busy: return .systemBlue
        case .needsYou: return .systemPurple
        case .ready: return .systemGreen
        case .failed: return .systemRed
        case .parked: return .systemOrange
        case .idle, .unknown: return .tertiaryLabelColor
        }
    }

    var label: String {
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
    var summary: String {
        [(needsYou, "needs you", "need you"), (failed, "failed", "failed"), (busy, "busy", "busy"),
         (ready, "ready", "ready"), (parked, "parked", "parked")]
            .filter { $0.0 > 0 }
            .map { "\($0.0) \($0.0 == 1 ? $0.1 : $0.2)" }
            .joined(separator: " · ")
    }

    /// The tone a group of workers shows: its most urgent one.
    var lead: Tone {
        if needsYou > 0 { return .needsYou }
        if failed > 0 { return .failed }
        if busy > 0 { return .busy }
        if ready > 0 { return .ready }
        if parked > 0 { return .parked }
        return .idle
    }
}

/// The top of the menu: is Quark running, and how is everything doing.
struct HeaderView: View {
    @ObservedObject var store: Store

    var body: some View {
        HStack(alignment: .firstTextBaseline) {
            VStack(alignment: .leading, spacing: 2) {
                Text("Quark").font(.headline)
                Text(line).font(.subheadline).foregroundStyle(.secondary).lineLimit(2)
            }
            Spacer()
            if case let .connected(h) = store.connection {
                Text("v\(h.version)").font(.caption).foregroundStyle(.tertiary)
            }
        }
        .padding(.horizontal, 14)
        .padding(.vertical, 6)
        .frame(width: rowWidth, alignment: .leading)
    }

    private var line: String {
        switch store.connection {
        case .connecting: return "Connecting to quarkd…"
        case .unreachable: return "quarkd isn't running"
        case .connected:
            guard let s = store.status else { return "Reading…" }
            if s.projects.isEmpty { return "No projects yet" }
            let counts = s.counts.summary
            if s.attention.isEmpty { return counts.isEmpty ? "Nothing needs you" : "Nothing needs you · \(counts)" }
            return counts.isEmpty ? "\(s.attention.count) waiting on you" : counts
        }
    }
}

/// One account's quota: a bar per window with when it resets.
struct QuotaRow: View {
    let account: Account
    let harness: String
    let now: Date

    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            HStack {
                Text(harness).font(.system(size: 13, weight: .medium))
                if !account.default || account.label != "Default" {
                    Text(account.label).font(.system(size: 12)).foregroundStyle(.secondary)
                }
                Spacer()
                if let p = account.shownRemaining {
                    Text("\(Format.percent(p)) left").font(.system(size: 12).monospacedDigit())
                        .foregroundStyle(p <= 10 ? .red : .secondary)
                }
            }
            if let note = account.quotaNote {
                Text(note).font(.caption).foregroundStyle(.secondary).lineLimit(1)
            } else {
                ForEach(account.quota.windows) { w in
                    WindowBar(window: w, now: now)
                }
            }
        }
        .padding(.horizontal, 14)
        .padding(.vertical, 4)
        .frame(width: rowWidth, alignment: .leading)
    }
}

struct WindowBar: View {
    let window: QuotaWindow
    let now: Date

    var body: some View {
        VStack(alignment: .leading, spacing: 2) {
            Meter(fraction: (window.percentRemaining ?? 0) / 100, tint: tint)
            HStack {
                Text(window.label.capitalized)
                Spacer()
                if let r = window.resetsAt { Text(Format.resets(r, now: now)) }
            }
            .font(.caption)
            .foregroundStyle(.secondary)
        }
    }

    private var tint: Color {
        let p = window.percentRemaining ?? 0
        return p <= 10 ? .red : p <= 30 ? .orange : .accentColor
    }
}

/// One host: health, CPU and memory.
struct HostRow: View {
    let host: Host

    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            HStack {
                Circle().fill(Color(nsColor: healthColor)).frame(width: 7, height: 7)
                Text(host.name).font(.system(size: 13, weight: .medium))
                Text(host.runtime == .local ? "This Mac" : host.runtime.rawValue.replacingOccurrences(of: "_", with: " "))
                    .font(.system(size: 12)).foregroundStyle(.secondary)
                Spacer()
                if let w = host.worktrees, w.inUse > 0 {
                    Text("\(w.inUse) in use").font(.caption).foregroundStyle(.secondary)
                }
            }
            if let r = host.latest {
                HStack(spacing: 10) {
                    LabeledMeter(label: "CPU", fraction: r.cpu)
                    LabeledMeter(label: "Memory", fraction: Double(r.memoryUsedBytes) / Double(max(r.memoryTotalBytes, 1)))
                }
            } else if let reason = host.health.reason {
                Text(reason).font(.caption).foregroundStyle(.secondary).lineLimit(1)
            }
        }
        .padding(.horizontal, 14)
        .padding(.vertical, 4)
        .frame(width: rowWidth, alignment: .leading)
    }

    private var healthColor: NSColor {
        switch host.health.status {
        case .healthy: return .systemGreen
        case .degraded: return .systemOrange
        case .unreachable: return .systemRed
        case .unknown: return .tertiaryLabelColor
        }
    }
}

struct LabeledMeter: View {
    let label: String
    let fraction: Double

    var body: some View {
        VStack(alignment: .leading, spacing: 2) {
            Meter(fraction: fraction, tint: fraction > 0.85 ? .orange : .accentColor)
            Text("\(label) \(Format.percent(fraction * 100))").font(.caption).foregroundStyle(.secondary)
        }
    }
}

struct Meter: View {
    let fraction: Double
    let tint: Color

    var body: some View {
        GeometryReader { g in
            ZStack(alignment: .leading) {
                Capsule().fill(Color.primary.opacity(0.1))
                Capsule().fill(tint).frame(width: g.size.width * min(max(fraction, 0), 1))
            }
        }
        .frame(height: 5)
    }
}

/// A menu item holding a SwiftUI view, sized to fit it.
func hostedItem<V: View>(_ view: V) -> NSMenuItem {
    let item = NSMenuItem()
    let host = NSHostingView(rootView: view)
    host.frame = NSRect(origin: .zero, size: host.fittingSize)
    item.view = host
    return item
}

/// A small filled circle in `color`, for menu item images.
func dot(_ color: NSColor, size: CGFloat = 8) -> NSImage {
    let image = NSImage(size: NSSize(width: size + 4, height: size + 4), flipped: false) { rect in
        color.setFill()
        NSBezierPath(ovalIn: rect.insetBy(dx: 2, dy: 2)).fill()
        return true
    }
    image.isTemplate = false
    return image
}

/// A two-line menu item title: the main text, and a smaller secondary line.
func twoLine(_ title: String, _ detail: String?) -> NSAttributedString {
    let s = NSMutableAttributedString(string: title, attributes: [.font: NSFont.menuFont(ofSize: 13)])
    if let detail, !detail.isEmpty {
        s.append(NSAttributedString(string: "\n" + detail, attributes: [
            .font: NSFont.menuFont(ofSize: 11),
            .foregroundColor: NSColor.secondaryLabelColor,
        ]))
    }
    return s
}
