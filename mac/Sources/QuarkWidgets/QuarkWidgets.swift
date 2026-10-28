import QuarkKit
import QuarkUI
import SwiftUI
import WidgetKit

// Desktop and Notification Center widgets: what needs you, projects, quota and
// hosts. Like QuarkBar they only read quarkd's API; QuarkBar asks WidgetKit to
// reload them when quarkd's status changes, and each also reads again every
// few minutes on its own.

@main
struct QuarkWidgets: WidgetBundle {
    var body: some Widget {
        NeedsYouWidget()
        ProjectsWidget()
        QuotaWidget()
        HostsWidget()
    }
}

// MARK: Data

/// One read of quarkd, as of `date`.
struct Snapshot: TimelineEntry {
    var date: Date
    var status: StatusView?
    var accounts: [Account] = []
    var hosts: [HostView] = []
    var harnessNames: [String: String] = [:]
    /// Why quarkd could not be read.
    var error: String?

    func harnessName(_ id: String) -> String { harnessNames[id] ?? id }

    static func load(accounts wantAccounts: Bool = false, hosts wantHosts: Bool = false) async -> Snapshot {
        let client = QuarkClient(base: QuarkClient.defaultBase)
        var s = Snapshot(date: Date())
        do {
            s.status = try await client.status()
        } catch {
            s.error = "quarkd isn't running"
            return s
        }
        if wantAccounts {
            s.accounts = ((try? await client.accounts()) ?? []).filter { $0.quota.state != .unsupported }
            let names = (try? await client.harnesses()) ?? []
            s.harnessNames = Dictionary(names.map { ($0.id, $0.name) }, uniquingKeysWith: { a, _ in a })
        }
        if wantHosts {
            s.hosts = (try? await client.hosts())?.hosts ?? []
        }
        return s
    }
}

struct Provider: TimelineProvider {
    var accounts = false
    var hosts = false

    func placeholder(in context: Context) -> Snapshot {
        Snapshot(date: Date())
    }

    func getSnapshot(in context: Context, completion: @escaping (Snapshot) -> Void) {
        Task { completion(await Snapshot.load(accounts: accounts, hosts: hosts)) }
    }

    func getTimeline(in context: Context, completion: @escaping (Timeline<Snapshot>) -> Void) {
        Task {
            let s = await Snapshot.load(accounts: accounts, hosts: hosts)
            // Sooner while quarkd is down, so the widget recovers when it starts.
            let next = Date().addingTimeInterval(s.error == nil ? 5 * 60 : 60)
            completion(Timeline(entries: [s], policy: .after(next)))
        }
    }
}

// MARK: Needs you

struct NeedsYouWidget: Widget {
    var body: some WidgetConfiguration {
        StaticConfiguration(kind: "systems.quark.needs-you", provider: Provider()) { s in
            NeedsYouView(s: s).containerBackground(.fill.tertiary, for: .widget)
        }
        .configurationDisplayName("Needs you")
        .description("Decisions, red PRs and stuck workers waiting on you.")
        .supportedFamilies([.systemSmall, .systemMedium, .systemLarge])
    }
}

struct NeedsYouView: View {
    let s: Snapshot
    @Environment(\.widgetFamily) var family

    var body: some View {
        if let error = s.error {
            Unreachable(error: error)
        } else {
            let items = s.status?.attention ?? []
            if family == .systemSmall {
                small(items)
            } else {
                list(items, max: family == .systemLarge ? 7 : 3)
            }
        }
    }

    private func small(_ items: [AttentionItem]) -> some View {
        VStack(alignment: .leading, spacing: 4) {
            Header(title: "Needs you")
            Spacer(minLength: 0)
            Text("\(items.count)")
                .font(.system(size: 40, weight: .semibold).monospacedDigit())
                .foregroundStyle(items.isEmpty ? Color.secondary : Tone.needsYou.color)
            Text(items.first?.title ?? "Nothing needs you")
                .font(.caption).foregroundStyle(.secondary).lineLimit(2)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .widgetURL(items.first.map { DeskLink($0).barURL } ?? DeskLink.home.barURL)
    }

    private func list(_ items: [AttentionItem], max: Int) -> some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack {
                Header(title: "Needs you")
                Spacer()
                if !items.isEmpty {
                    Text("\(items.count)").font(.headline.monospacedDigit()).foregroundStyle(Tone.needsYou.color)
                }
            }
            if items.isEmpty {
                Spacer()
                Text("Nothing needs you").font(.callout).foregroundStyle(.secondary)
                Spacer()
            } else {
                ForEach(items.prefix(max)) { item in
                    Link(destination: DeskLink(item).barURL) {
                        HStack(alignment: .top, spacing: 8) {
                            Image(systemName: item.kind.symbol).foregroundStyle(Tone.needsYou.color)
                            VStack(alignment: .leading, spacing: 1) {
                                Text(item.title).font(.callout).lineLimit(1)
                                Text([item.projectName, item.kind.label, item.sinceDate.map { ago($0, now: s.date) }]
                                    .compactMap { $0 }.joined(separator: " · "))
                                    .font(.caption).foregroundStyle(.secondary).lineLimit(1)
                            }
                        }
                    }
                }
                Spacer(minLength: 0)
                if items.count > max {
                    Text("and \(items.count - max) more").font(.caption).foregroundStyle(.secondary)
                }
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }
}

// MARK: Projects

struct ProjectsWidget: Widget {
    var body: some WidgetConfiguration {
        StaticConfiguration(kind: "systems.quark.projects", provider: Provider()) { s in
            ProjectsView(s: s).containerBackground(.fill.tertiary, for: .widget)
        }
        .configurationDisplayName("Projects")
        .description("Every project and how its agents are doing.")
        .supportedFamilies([.systemMedium, .systemLarge])
    }
}

struct ProjectsView: View {
    let s: Snapshot
    @Environment(\.widgetFamily) var family

    var body: some View {
        if let error = s.error {
            Unreachable(error: error)
        } else {
            let projects = s.status?.projects ?? []
            let max = family == .systemLarge ? 8 : 3
            VStack(alignment: .leading, spacing: 6) {
                HStack {
                    Header(title: "Projects")
                    Spacer()
                    if let counts = s.status?.counts, !counts.summary.isEmpty {
                        Text(counts.summary).font(.caption).foregroundStyle(.secondary).lineLimit(1)
                    }
                }
                if projects.isEmpty {
                    Spacer()
                    Text("No projects yet").font(.callout).foregroundStyle(.secondary)
                    Spacer()
                } else {
                    ForEach(projects.prefix(max)) { p in
                        Link(destination: DeskLink.project(p.id).barURL) {
                            HStack(spacing: 8) {
                                Circle().fill(p.tone.color).frame(width: 8, height: 8)
                                VStack(alignment: .leading, spacing: 1) {
                                    Text(p.name).font(.callout.weight(.medium)).lineLimit(1)
                                    Text(p.line(now: s.date)).font(.caption).foregroundStyle(.secondary).lineLimit(1)
                                }
                                Spacer(minLength: 0)
                                if p.attention > 0 {
                                    Text("\(p.attention)").font(.caption.weight(.semibold).monospacedDigit())
                                        .foregroundStyle(Tone.needsYou.color)
                                }
                            }
                        }
                    }
                    Spacer(minLength: 0)
                }
            }
            .frame(maxWidth: .infinity, alignment: .leading)
        }
    }
}

// MARK: Quota

struct QuotaWidget: Widget {
    var body: some WidgetConfiguration {
        StaticConfiguration(kind: "systems.quark.quota", provider: Provider(accounts: true)) { s in
            QuotaView(s: s).containerBackground(.fill.tertiary, for: .widget)
        }
        .configurationDisplayName("Quota")
        .description("What each account has left, and when it resets.")
        .supportedFamilies([.systemSmall, .systemMedium])
    }
}

struct QuotaView: View {
    let s: Snapshot
    @Environment(\.widgetFamily) var family

    var body: some View {
        if let error = s.error {
            Unreachable(error: error)
        } else {
            VStack(alignment: .leading, spacing: 6) {
                Header(title: "Quota")
                if s.accounts.isEmpty {
                    Spacer()
                    Text("No quota reported").font(.callout).foregroundStyle(.secondary)
                    Spacer()
                } else {
                    ForEach(s.accounts.prefix(family == .systemSmall ? 2 : 4)) { a in
                        VStack(alignment: .leading, spacing: 2) {
                            HStack {
                                Text(s.harnessName(a.harness)).font(.caption.weight(.medium)).lineLimit(1)
                                Spacer(minLength: 4)
                                Text(a.shownRemaining.map { Format.percent($0) } ?? "–")
                                    .font(.caption.monospacedDigit()).foregroundStyle(.secondary)
                            }
                            Meter(fraction: (a.shownRemaining ?? 0) / 100, tint: quotaTint(a.shownRemaining ?? 0))
                            if family != .systemSmall, let note = resetNote(a) {
                                Text(note).font(.caption2).foregroundStyle(.secondary).lineLimit(1)
                            }
                        }
                    }
                    Spacer(minLength: 0)
                }
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            .widgetURL(DeskLink.accounts.barURL)
        }
    }

    /// When the window that limits the account resets, or why there are no numbers.
    private func resetNote(_ a: Account) -> String? {
        if let note = a.quotaNote { return note }
        let limiting = a.quota.windows.min { ($0.percentRemaining ?? 100) < ($1.percentRemaining ?? 100) }
        guard let w = limiting, let at = w.resetsAt else { return nil }
        return "\(w.label.capitalized) \(Format.resets(at, now: s.date))"
    }
}

// MARK: Hosts

struct HostsWidget: Widget {
    var body: some WidgetConfiguration {
        StaticConfiguration(kind: "systems.quark.hosts", provider: Provider(hosts: true)) { s in
            HostsView(s: s).containerBackground(.fill.tertiary, for: .widget)
        }
        .configurationDisplayName("Hosts")
        .description("Each host's health, CPU and memory.")
        .supportedFamilies([.systemSmall, .systemMedium])
    }
}

struct HostsView: View {
    let s: Snapshot
    @Environment(\.widgetFamily) var family

    var body: some View {
        if let error = s.error {
            Unreachable(error: error)
        } else {
            VStack(alignment: .leading, spacing: 6) {
                Header(title: "Hosts")
                if s.hosts.isEmpty {
                    Spacer()
                    Text("No hosts reported").font(.callout).foregroundStyle(.secondary)
                    Spacer()
                } else {
                    ForEach(s.hosts.prefix(family == .systemSmall ? 1 : 3)) { h in
                        VStack(alignment: .leading, spacing: 3) {
                            HStack(spacing: 6) {
                                Circle().fill(health(h)).frame(width: 7, height: 7)
                                Text(h.name).font(.caption.weight(.medium)).lineLimit(1)
                            }
                            if let r = h.latest {
                                bar("CPU", r.cpu)
                                bar("Memory", Double(r.memoryUsedBytes) / Double(max(r.memoryTotalBytes, 1)))
                            } else if let reason = h.health.reason {
                                Text(reason).font(.caption2).foregroundStyle(.secondary).lineLimit(1)
                            }
                        }
                    }
                    Spacer(minLength: 0)
                }
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            .widgetURL(DeskLink.hosts.barURL)
        }
    }

    private func bar(_ label: String, _ fraction: Double) -> some View {
        HStack(spacing: 6) {
            Text(label).font(.caption2).foregroundStyle(.secondary).frame(width: 44, alignment: .leading)
            Meter(fraction: fraction, tint: fraction > 0.85 ? .orange : .accentColor)
        }
    }

    private func health(_ h: HostView) -> Color {
        switch h.health.status {
        case .healthy: return .green
        case .degraded: return .orange
        case .unreachable: return .red
        case .unknown: return .secondary
        }
    }
}

// MARK: Parts

struct Header: View {
    let title: String

    var body: some View {
        HStack(spacing: 4) {
            Image(systemName: "atom").font(.caption)
            Text(title).font(.caption.weight(.semibold))
        }
        .foregroundStyle(.secondary)
    }
}

struct Unreachable: View {
    let error: String

    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            Header(title: "Quark")
            Spacer()
            Text(error).font(.callout).foregroundStyle(.secondary)
            Spacer()
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }
}
