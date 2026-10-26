import AppKit
import QuarkKit
import QuarkUI
import SwiftUI

/// Width of the rich rows hosted in the menu.
let rowWidth: CGFloat = 320

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

    private var tint: Color { quotaTint(window.percentRemaining ?? 0) }
}

/// One host: health, CPU and memory.
struct HostRow: View {
    let host: HostView

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
