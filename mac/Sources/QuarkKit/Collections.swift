import Foundation

/// `GET /v1/collections`: a named set of Projects the desktop app's left list
/// groups by. A Project may be in several, or none.
public struct ProjectCollection: Codable, Equatable, Identifiable, Sendable {
    public var id: String
    public var name: String
    /// Where it sits among the collections, ascending; ties break by name.
    public var position: Int64
    /// Folded shut in the left list, showing its heading alone.
    public var collapsed: Bool?
    /// Its Projects, in the order the left list shows them.
    public var projects: [String]

    public init(id: String, name: String, position: Int64, collapsed: Bool? = nil, projects: [String]) {
        self.id = id
        self.name = name
        self.position = position
        self.collapsed = collapsed
        self.projects = projects
    }
}

/// `GET /v1/preferences`: how every client shows what is there.
public struct Preferences: Codable, Equatable, Sendable {
    /// Whether the Projects list groups under collections.
    public var groupByCollection: Bool?
}

/// A heading in the menu's Projects list with the Projects under it.
public struct ProjectSection: Equatable, Sendable {
    /// The collection's id; nil for "Ungrouped" and for the flat list.
    public var id: String?
    /// The heading; nil for the flat list, which keeps the plain "Projects".
    public var name: String?
    public var collapsed: Bool
    public var projects: [ProjectSummary]
}

/// The name of the section holding Projects in no collection, as in the app.
public let ungroupedName = "Ungrouped"

/// The Projects list's sections, grouped as the desktop app's left list
/// groups them (app/src/shell/collections.ts): each collection in its order
/// with its Projects in the collection's order, a Project in several under
/// each, then "Ungrouped" for the rest. Collections with nothing shown are
/// left out, since a menu has no use for an empty heading. With grouping off,
/// or no collections, one flat section in `projects`' order.
public func projectSections(_ projects: [ProjectSummary], collections: [ProjectCollection], grouped: Bool) -> [ProjectSection] {
    guard grouped, !collections.isEmpty else {
        return [ProjectSection(id: nil, name: nil, collapsed: false, projects: projects)]
    }
    let byId = Dictionary(projects.map { ($0.id, $0) }, uniquingKeysWith: { a, _ in a })
    var placed = Set<String>()
    var out: [ProjectSection] = []
    let ordered = collections.sorted { ($0.position, $0.name) < ($1.position, $1.name) }
    for c in ordered {
        let inside = c.projects.compactMap { byId[$0] }
        placed.formUnion(inside.map(\.id))
        if !inside.isEmpty {
            out.append(ProjectSection(id: c.id, name: c.name, collapsed: c.collapsed ?? false, projects: inside))
        }
    }
    let rest = projects.filter { !placed.contains($0.id) }
    if !rest.isEmpty {
        out.append(ProjectSection(id: nil, name: ungroupedName, collapsed: false, projects: rest))
    }
    return out
}
