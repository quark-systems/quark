import AppKit
import QuarkKit
import QuarkUI
import SwiftUI

/// The narrowest a rich row hosted in the menu may be. `fitHostedRows` widens rows
/// to the menu's width once every item is in.
let rowWidth: CGFloat = 320

/// Where text sits in a menu item, so hosted rows line up with plain ones: the
/// leading inset is past the menu's checkmark column, the trailing one ends where
/// submenu arrows and shortcuts do.
enum MenuInset {
    static let leading: CGFloat = 27
    static let trailing: CGFloat = 18
}

extension View {
    /// Lays out a hosted row with plain items' insets.
    func menuRow(vertical: CGFloat = 4) -> some View {
        padding(.leading, MenuInset.leading)
            .padding(.trailing, MenuInset.trailing)
            .padding(.vertical, vertical)
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
        .menuRow(vertical: 6)
    }

    private var line: String {
        switch store.connection {
        case .connecting: return "Connecting to quarkd…"
        case .unreachable: return "quarkd isn't running"
        case let .connected(h):
            if let mismatch = h.apiMismatch { return mismatch }
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
                if let plan = account.quota.plan {
                    Text(plan).font(.system(size: 12)).foregroundStyle(.secondary).lineLimit(1)
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
        .menuRow()
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
            if let usage = Format.usage(window) {
                Text(usage).font(.caption.monospacedDigit())
                    .foregroundStyle(window.overage ?? 0 > 0 ? .orange : .secondary)
                    .lineLimit(1)
            }
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
        .menuRow()
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
    item.view = RowHost(view)
    return item
}

/// A SwiftUI row in the menu, laid out at a fixed width (a menu does not lay out
/// its views itself).
final class RowHost: NSHostingView<AnyView> {
    private let content: AnyView

    init<V: View>(_ view: V) {
        content = AnyView(view)
        super.init(rootView: content)
        fit(width: rowWidth)
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("not used") }

    @available(*, unavailable)
    required init(rootView: AnyView) { fatalError("not used") }

    func fit(width: CGFloat) {
        rootView = AnyView(content.frame(width: width, alignment: .leading))
        frame = NSRect(origin: .zero, size: fittingSize)
    }
}

/// Widens every hosted row of `menu` to the menu's width, so bars and right-hand
/// labels end where submenu arrows and shortcuts do.
func fitHostedRows(_ menu: NSMenu) {
    let width = max(rowWidth, menu.size.width)
    for item in menu.items {
        (item.view as? RowHost)?.fit(width: width)
    }
}

/// A two-line menu item title: the main text, and a smaller secondary line,
/// each clipped so a long title cannot stretch the whole menu. With `dot`, a
/// colored dot leads the first line and the second line lines up under the text.
/// (Menu item images do not show next to multi-line titles on every macOS, so
/// the dot is part of the text.)
func twoLine(_ title: String, _ detail: String?, dot: NSColor? = nil) -> NSAttributedString {
    let s = NSMutableAttributedString()
    let titleFont = NSFont.menuFont(ofSize: 13)
    let para = NSMutableParagraphStyle()
    if let dot {
        let lead = NSAttributedString(string: "\u{25CF}  ", attributes: [
            .font: NSFont.menuFont(ofSize: 10), .foregroundColor: dot, .baselineOffset: 1,
        ])
        para.headIndent = ceil(lead.size().width)
        s.append(lead)
    }
    s.append(NSAttributedString(string: clip(title, 56), attributes: [.font: titleFont]))
    if let detail, !detail.isEmpty {
        // A line separator, not a newline, keeps one paragraph so headIndent applies.
        s.append(NSAttributedString(string: "\u{2028}" + clip(detail, 64), attributes: [
            .font: NSFont.menuFont(ofSize: 11),
            .foregroundColor: NSColor.secondaryLabelColor,
        ]))
    }
    s.addAttribute(.paragraphStyle, value: para, range: NSRange(location: 0, length: s.length))
    return s
}

/// `s` cut to `max` characters with an ellipsis.
func clip(_ s: String, _ max: Int) -> String {
    let flat = s.replacingOccurrences(of: "\n", with: " ")
    return flat.count <= max ? flat : String(flat.prefix(max - 1)).trimmingCharacters(in: .whitespaces) + "\u{2026}"
}
