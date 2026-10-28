import Foundation
import XCTest
@testable import QuarkKit

final class QuarkKitTests: XCTestCase {
    func fixture(_ name: String) throws -> Data {
        let url = try XCTUnwrap(Bundle.module.url(forResource: name, withExtension: "json", subdirectory: "Fixtures"))
        return try Data(contentsOf: url)
    }

    func testDecodesAccounts() throws {
        let accounts = try JSONDecoder.quark.decode([Account].self, from: fixture("accounts"))
        XCTAssertEqual(accounts.count, 3)
        let claude = accounts[0]
        XCTAssertTrue(claude.default)
        XCTAssertEqual(claude.activeTasks, 2)
        XCTAssertEqual(claude.quota.state, .known)
        XCTAssertEqual(claude.shownRemaining, 38)
        XCTAssertEqual(claude.quota.windows.map(\.label), ["session", "week"])
        XCTAssertNotNil(claude.quota.windows[1].resetsAt, "fractional seconds parse")
        XCTAssertEqual(accounts[1].health.state, .notConfigured)
        XCTAssertNil(accounts[1].shownRemaining)
        XCTAssertEqual(accounts[1].quotaNote, "bob quota reader failed")
        // Values a newer quarkd might send decode as unknown.
        XCTAssertEqual(accounts[2].health.state, .unknown)
        XCTAssertEqual(accounts[2].quota.state, .unknown)
        XCTAssertEqual(lowestRemaining(accounts), 38)
    }

    func testDecodesHosts() throws {
        let view = try JSONDecoder.quark.decode(HostsView.self, from: fixture("hosts"))
        XCTAssertEqual(view.hosts.map(\.runtime), [.local, .privateCloud])
        let local = view.hosts[0]
        XCTAssertEqual(local.health.status, .healthy)
        XCTAssertEqual(local.latest?.cpu, 0.31)
        XCTAssertEqual(local.worktrees?.inUse, 2)
        XCTAssertEqual(local.projects.first?.projectName, "Quark")
        XCTAssertEqual(view.hosts[1].health.status, .unreachable)
        XCTAssertNil(view.hosts[1].latest)
    }

    func testDecodesStatus() throws {
        let status = try JSONDecoder.quark.decode(StatusView.self, from: fixture("status"))
        XCTAssertEqual(status.attention.map(\.kind), [.pullRequest, .decision, .unknown])
        XCTAssertNil(status.attention[2].sinceDate)
        XCTAssertNotNil(status.attention[0].sinceDate)
        XCTAssertEqual(status.counts.needsYou, 1)
        let p = try XCTUnwrap(status.projects.first)
        XCTAssertEqual(p.failingPullRequests, 1)
        XCTAssertEqual(p.workers.first?.tone, .busy)
        XCTAssertEqual(p.workers.first?.note, "building QuarkKit")
    }

    func testDecodesHealthAndEvent() throws {
        let health = try JSONDecoder.quark.decode(Health.self, from: Data(#"{"status":"ok","version":"0.1.0","engine":"native","last_seq":42}"#.utf8))
        XCTAssertEqual(health.lastSeq, 42)
        let event = try JSONDecoder.quark.decode(Event.self, from: Data(#"{"seq":43,"project_id":null,"type":"account.quota_changed","ts":"2026-10-26T22:00:00Z","payload":{"x":1}}"#.utf8))
        XCTAssertEqual(event.type, "account.quota_changed")
        XCTAssertNil(event.notice)
        let notice = try JSONDecoder.quark.decode(Event.self, from: Data(#"{"seq":44,"project_id":"prj_1","type":"notice.posted","ts":"2026-10-26T22:00:00Z","payload":{"project_id":"prj_1","kind":"now","text":"menubar-app: done: PR is green","task_id":"tsk_1","occasion":"done"}}"#.utf8))
        XCTAssertEqual(notice.notice?.occasion, "done")
        XCTAssertEqual(notice.notice?.taskId, "tsk_1")
    }

    func testFormat() {
        let now = Date(timeIntervalSince1970: 1_000_000)
        XCTAssertEqual(Format.resets(now.addingTimeInterval(2 * 3600 + 14 * 60), now: now), "resets in 2h 14m")
        XCTAssertEqual(Format.resets(now.addingTimeInterval(3 * 86_400 + 4 * 3600 + 5), now: now), "resets in 3d 4h")
        XCTAssertEqual(Format.resets(now.addingTimeInterval(20), now: now), "resets in 1m")
        XCTAssertEqual(Format.resets(now.addingTimeInterval(-5), now: now), "resets now")
        XCTAssertEqual(Format.percent(37.6), "38%")
        let month = QuotaWindow(id: "monthly", label: "month", used: 435.5, limit: 1500, unit: "Bobcoins")
        XCTAssertEqual(Format.usage(month), "435.5 of 1,500 Bobcoins used")
        let over = QuotaWindow(id: "monthly", label: "month", used: 1100, limit: 1000, unit: "credits", overage: 100)
        XCTAssertEqual(Format.usage(over), "1,100 of 1,000 credits used, 100 over (overage)")
        XCTAssertNil(Format.usage(QuotaWindow(id: "weekly", label: "week", percentRemaining: 40)))
        XCTAssertEqual(Format.checked(now.addingTimeInterval(-180), now: now), "checked 3m ago")
    }

    func testDeskLinks() throws {
        let status = try JSONDecoder.quark.decode(StatusView.self, from: fixture("status"))
        XCTAssertEqual(DeskLink(status.attention[0]).url.absoluteString, "quark://open/pr/pr_1")
        XCTAssertEqual(DeskLink(status.attention[1]).path, "inbox/dec_1")
        XCTAssertEqual(DeskLink.worker("a/b").path, "t/a%2Fb")
        XCTAssertEqual(DeskLink.home.url.absoluteString, "quark://open/")
        XCTAssertEqual(DeskLink.hosts.barURL.absoluteString, "quarkbar://open/hosts")
        XCTAssertEqual(DeskLink.deskURL(fromBar: DeskLink.worker("a/b").barURL)?.absoluteString, "quark://open/t/a%2Fb")
        XCTAssertNil(DeskLink.deskURL(fromBar: DeskLink.home.url))
    }

    func testClientURLs() {
        let client = QuarkClient(base: URL(string: "http://127.0.0.1:7380")!)
        XCTAssertEqual(client.eventsURL(cursor: 7).absoluteString, "ws://127.0.0.1:7380/v1/events?cursor=7")
        XCTAssertEqual(QuarkClient.configuredBase(["QUARK_DAEMON": "http://10.0.0.2:9000"]).absoluteString, "http://10.0.0.2:9000")
        XCTAssertEqual(QuarkClient.configuredBase([:]), QuarkClient.defaultBase)
    }
}
