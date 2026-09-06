import XCTest
@testable import CcsMenu

final class CcsTests: XCTestCase {
    let home = URL(fileURLWithPath: "/Users/you")

    func testTheBinaryIsLookedForInTheCargoBinThenTheSystemPlaces() {
        let found = ccsCandidates(environment: [:], home: home).map(\.path)
        XCTAssertEqual(found, ["/Users/you/.cargo/bin/ccs", "/usr/local/bin/ccs", "/opt/homebrew/bin/ccs"])
    }

    func testAnEnvironmentOverrideComesFirst() {
        let found = ccsCandidates(environment: ["CCS_BINARY": "/tmp/ccs"], home: home).map(\.path)
        XCTAssertEqual(found.first, "/tmp/ccs")
        XCTAssertEqual(found.count, 4)
    }

    func testAnEmptyOverrideIsNoOverride() {
        let found = ccsCandidates(environment: ["CCS_BINARY": ""], home: home)
        XCTAssertEqual(found.count, 3)
    }

    /// A stand-in `ccs`: a shell script that prints a listing for `ls --json`
    /// and refuses everything else the way the real one does.
    private func fakeCcs() throws -> URL {
        let dir = FileManager.default.temporaryDirectory.appendingPathComponent("ccs-menu-\(getpid())")
        try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        let script = dir.appendingPathComponent("ccs")
        let body = """
        #!/bin/sh
        if [ "$1" = "ls" ] && [ "$2" = "--cached" ]; then
          printf '%s' '[{"slug":"a","email":"a@x","plan":"pro","active":true,"limits":[]}]'
          exit 0
        fi
        if [ "$1" = "use" ]; then
          echo "switched to $2"
          exit 0
        fi
        echo "ccs: unknown command \\"$1\\"" >&2
        exit 1
        """
        try body.write(to: script, atomically: true, encoding: .utf8)
        try FileManager.default.setAttributes([.posixPermissions: 0o755], ofItemAtPath: script.path)
        return script
    }

    /// The app reads what the last poll wrote down rather than polling: a
    /// listing that went to the network would spend the very limits it shows.
    func testListingReadsTheCacheRatherThanPolling() async throws {
        let ccs = Ccs(binary: try fakeCcs())
        let accounts = try await ccs.list()
        XCTAssertEqual(accounts.map(\.slug), ["a"])
    }

    func testAnUncachedListingIsWhatTheBinaryRefuses() async throws {
        let ccs = Ccs(binary: try fakeCcs())
        _ = try await ccs.list()  // cached: fine
        do {
            _ = try await ccs.run(["ls", "--json"])
            XCTFail("the fake only answers a cached listing")
        } catch is CcsError {}
    }

    func testSwitchingHandsBackWhatTheBinarySaid() async throws {
        let ccs = Ccs(binary: try fakeCcs())
        let said = try await ccs.use("work", force: false)
        XCTAssertEqual(said, "switched to work")
    }

    func testAFailureCarriesTheBinarysOwnMessage() async throws {
        let ccs = Ccs(binary: try fakeCcs())
        do {
            _ = try await ccs.run(["frobnicate"])
            XCTFail("should have failed")
        } catch let error as CcsError {
            XCTAssertEqual(error.message, "unknown command \"frobnicate\"")
        }
    }
}
