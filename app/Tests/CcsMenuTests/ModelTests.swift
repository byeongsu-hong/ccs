import XCTest
@testable import CcsMenu

final class ModelTests: XCTestCase {
    /// What `ccs ls --json` prints, trimmed to two accounts.
    static let listing = """
    [
      {"slug": "you_at_example.com", "email": "you@example.com", "plan": "max20x", "active": true,
       "polled_at": "2026-09-06T12:00:00.123456Z",
       "limits": [
         {"kind": "session", "percent": 16.0, "severity": "normal",
          "resets_at": "2026-09-06T17:00:00.387644+00:00", "scope": null},
         {"kind": "weekly_all", "percent": 29.0, "severity": "normal",
          "resets_at": "2026-09-10T08:00:00.387662+00:00", "scope": null},
         {"kind": "weekly_scoped", "percent": 52.0, "severity": "normal",
          "resets_at": "2026-09-10T08:00:00.388303+00:00",
          "scope": {"model": {"display_name": "Fable"}}}
       ]},
      {"slug": "alt_at_example.com", "email": "alt@example.com", "plan": "pro", "active": false,
       "limits": [{"kind": "session", "percent": 100.0, "severity": null, "resets_at": null, "scope": null}]}
    ]
    """.data(using: .utf8)!

    func testAListingDecodesIntoAccountsWithTheirLimits() throws {
        let accounts = try decodeAccounts(Self.listing)
        XCTAssertEqual(accounts.map(\.slug), ["you_at_example.com", "alt_at_example.com"])
        XCTAssertEqual(accounts[0].email, "you@example.com")
        XCTAssertEqual(accounts[0].plan, "max20x")
        XCTAssertTrue(accounts[0].active)
        XCTAssertEqual(accounts[0].limits.count, 3)
        XCTAssertEqual(accounts[0].limits[2].percent, 52)
    }

    func testColumnsFollowTheCli() throws {
        let limits = try decodeAccounts(Self.listing)[0].limits
        XCTAssertEqual(limits.map(\.column), ["session", "weekly", "Fable"])
        XCTAssertEqual(limit(kind: "monthly_scoped").column, "monthly scoped")
    }

    func testAResetTimeWithSixFractionalDigitsParses() throws {
        let session = try decodeAccounts(Self.listing)[0].limits[0]
        let expected = ISO8601DateFormatter().date(from: "2026-09-06T17:00:00Z")!
        XCTAssertEqual(session.resetsAt, expected)
    }

    func testWhenAReadingWasTakenIsDecodedWhenTheListingSaysSo() throws {
        let accounts = try decodeAccounts(Self.listing)
        XCTAssertEqual(accounts[0].polledAt, ISO8601DateFormatter().date(from: "2026-09-06T12:00:00Z"))
        XCTAssertNil(accounts[1].polledAt)
    }

    func testAMissingResetTimeIsNilNotAFailure() throws {
        XCTAssertNil(try decodeAccounts(Self.listing)[1].limits[0].resetsAt)
    }

    func testSpentIsSpentWhateverTheSeverity() {
        XCTAssertEqual(limit(percent: 100, severity: "normal").health, .spent)
        XCTAssertEqual(limit(percent: 140).health, .spent)
    }

    func testSeverityWinsUnderTheCap() {
        XCTAssertEqual(limit(percent: 12, severity: "critical").health, .warn)
        XCTAssertEqual(limit(percent: 90, severity: "normal").health, .ok)
    }

    func testThePercentageDecidesWhenSeverityIsAbsent() {
        XCTAssertEqual(limit(percent: 85).health, .warn)
        XCTAssertEqual(limit(percent: 20).health, .ok)
    }

    func testAnAccountIsSpentWhenAnyLimitIs() throws {
        let accounts = try decodeAccounts(Self.listing)
        XCTAssertFalse(accounts[0].spent)
        XCTAssertTrue(accounts[1].spent)
        XCTAssertEqual(accounts[0].session?.percent, 16)
    }

    func testUntilReadsLikeTheCli() {
        let now = Date(timeIntervalSince1970: 1_000_000)
        XCTAssertEqual(until(now.addingTimeInterval(3 * 3600 + 54 * 60 + 20), now: now), "3h54m")
        XCTAssertEqual(until(now.addingTimeInterval(2 * 86400 + 11 * 3600), now: now), "2d11h")
        XCTAssertEqual(until(now.addingTimeInterval(7 * 60), now: now), "7m")
        XCTAssertEqual(until(now.addingTimeInterval(-5), now: now), "now")
    }

    private func limit(kind: String = "session", percent: Double = 0, severity: String? = nil) -> Limit {
        Limit(kind: kind, percent: percent, severity: severity, resetsAt: nil, scope: nil)
    }
}
