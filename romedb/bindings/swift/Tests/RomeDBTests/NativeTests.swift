import XCTest
import Foundation
@testable import RomeDB

final class NativeTests: XCTestCase {
    func testNativeContract() throws {
        let path = FileManager.default.temporaryDirectory.appendingPathComponent("romedb-swift-\(UUID()).db").path
        var db = try Database(path: path)
        defer {
            try? db.close()
            for suffix in ["", "-wal", "-shm"] { try? FileManager.default.removeItem(atPath: path + suffix) }
        }
        let result = try db.execute("SELECT $n,$b,$z,$nil,$a,$o", parameters: [
            "$n": .integer(Int64.max), "$b": .boolean(false), "$z": .number(-0.0),
            "$nil": .null, "$a": .array([]), "$o": .object([:])
        ])
        let rows = try XCTUnwrap(result["rows"] as? [[[String: Any]]])
        XCTAssertEqual(rows[0][0]["value"] as? String, String(Int64.max))
        XCTAssertEqual(rows[0][2]["value"] as? String, "8000000000000000")
        let fixture = ProcessInfo.processInfo.environment["ROMEDB_FIXTURE"] ?? URL(fileURLWithPath: #filePath).deletingLastPathComponent().deletingLastPathComponent().deletingLastPathComponent().appendingPathComponent("testdata/native-client.json").path
        let steps = try XCTUnwrap(JSONSerialization.jsonObject(with: Data(contentsOf: URL(fileURLWithPath: fixture))) as? [[String: Any]])
        func equal(_ a: Any?, _ b: Any?) -> Bool {
            guard let a, let b else { return false }
            let opts: JSONSerialization.WritingOptions = [.fragmentsAllowed, .sortedKeys]
            return (try? JSONSerialization.data(withJSONObject: a, options: opts)) == (try? JSONSerialization.data(withJSONObject: b, options: opts))
        }
        for (index, step) in steps.enumerated() {
            if step["reopen"] as? Bool == true {
                try db.close(); try db.close()
                XCTAssertThrowsError(try db.execute("SELECT 1"))
                db = try Database(path: path); continue
            }
            do {
                let r = try db.call(try XCTUnwrap(step["request"] as? [String: Any]), timeoutMs: (step["timeout_ms"] as? NSNumber)?.int64Value ?? -1)
                XCTAssertNil(step["error"], "step \(index): expected error")
                let execution = try XCTUnwrap(r["execution"] as? [String: Any])
                if let result = execution["result"] as? [String: Any] {
                    for key in ["rows", "columns"] where step[key] != nil {
                        XCTAssertTrue(equal(result[key], step[key]), "step \(index): \(key): \(result)")
                    }
                    if let subset = step["result_subset"] as? [String: Any] {
                        for (key, value) in subset { XCTAssertTrue(equal(result[key], value), "step \(index): \(key)") }
                    }
                }
                if step["result"] != nil { XCTAssertTrue(equal(execution["result"], step["result"]), "step \(index): result") }
            } catch let error as RomeDBError {
                XCTAssertEqual(error.code, step["error"] as? String, "step \(index): \(error)")
                if step["transaction"] != nil { XCTAssertTrue(equal(error.transaction, step["transaction"]), "step \(index): transaction") }
            }
        }
        print("\(steps.count) Swift native contract steps passed")
    }
}
