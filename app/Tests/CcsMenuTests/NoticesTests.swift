import XCTest
@testable import CcsMenu

final class NoticesTests: XCTestCase {
    func testAWatcherLineBecomesANoticeOfItsKind() {
        XCTAssertEqual(
            parseNotice("12:00:01 session-high: you@x.com has crossed 90% of its session"),
            .sessionHigh("you@x.com has crossed 90% of its session"))
        XCTAssertEqual(parseNotice("12:00:01 session-reset: alt@x.com is back"), .sessionReset("alt@x.com is back"))
        XCTAssertEqual(parseNotice("12:00:01 weekly-reset: alt@x.com weekly is back"), .weeklyReset("alt@x.com weekly is back"))
        XCTAssertEqual(parseNotice("12:00:01 rotate: switched to alt@x.com (alt)"), .rotated("switched to alt@x.com (alt)"))
    }

    func testTheHeardSuffixIsDroppedFromANotice() {
        XCTAssertEqual(
            parseNotice("12:00:01 session-high: you@x.com has crossed 90%; 2 sessions told"),
            .sessionHigh("you@x.com has crossed 90%"))
    }

    func testOtherLinesAreNotNotices() {
        XCTAssertNil(parseNotice("12:00:01 poll failed: token rejected"))
        XCTAssertNil(parseNotice("12:00:01 rotating between a, b"))
        XCTAssertNil(parseNotice("12:00:01 POST /v1/messages 200 as a 1.2s"))
        XCTAssertNil(parseNotice(""))
    }

    func testTheGatewaysArgumentsCarryThePortAndThePoolWhenThereIsOne() {
        XCTAssertEqual(serveArguments(port: 4141, pool: []), ["serve", "--port", "4141"])
        XCTAssertEqual(serveArguments(port: 8080, pool: ["a", "b"]), ["serve", "--port", "8080", "--rotate", "a,b"])
    }

    func testTheWatchersArgumentsCarryThePoolWhenThereIsOne() {
        XCTAssertEqual(watchArguments(pool: []), ["watch"])
        XCTAssertEqual(watchArguments(pool: ["a"]), ["watch", "--rotate", "a"])
    }

    func testANoticeHasATitleAndABody() {
        let notice = Notice.sessionHigh("you@x.com has crossed 90%")
        XCTAssertEqual(notice.title, "Session running high")
        XCTAssertEqual(notice.body, "you@x.com has crossed 90%")
        XCTAssertEqual(Notice.rotated("switched to a").title, "Rotated")
    }
}
