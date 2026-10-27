import Foundation
import CRomeDB

public struct RomeDBError: Error {
    public let diagnostic: [String: Any]
    public let transaction: [String: Any]?
    public var code: String { diagnostic["code"] as? String ?? "FDB_PROTOCOL" }
}
public struct Value {
    public let wire: [String: Any]
    public init(type: String, value: Any) { wire = ["type": type, "value": value] }
    private init(wire: [String: Any]) { self.wire = wire }
    public static var null: Value { Value(wire: ["type": "Null"]) }
    public static func integer(_ n: Int64) -> Value { Value(type: "Integer", value: String(n)) }
    public static func string(_ s: String) -> Value { Value(type: "String", value: s) }
    public static func boolean(_ b: Bool) -> Value { Value(type: "Boolean", value: b) }
    public static func number(_ n: Double) -> Value { Value(type: "Number", value: String(format: "%016llx", n.bitPattern)) }
    public static func array(_ values: [Value]) -> Value { Value(type: "Array", value: values.map(\.wire)) }
    public static func object(_ fields: [String: Value]) -> Value { Value(type: "Object", value: fields.mapValues(\.wire)) }
    public static func record(_ table: String, key: Value) -> Value { Value(type: "Record", value: ["table": table, "key": key.wire]) }
    public static func binary(_ bytes: Data) -> Value { Value(type: "Binary", value: Array(bytes)) }
    public static func vector(_ bytes: Data) -> Value { Value(type: "Vector", value: Array(bytes)) }
}

/// Synchronous native connection. Calls serialize in the engine. Group multi-call
/// transactions in one caller-owned serial executor. No browser or HTTP runtime.
public final class Database {
    private let handle: UInt64
    private static func protocolError(_ message: String) -> RomeDBError {
        RomeDBError(diagnostic: ["code": "FDB_PROTOCOL", "message": message], transaction: nil)
    }
    private static func receive(_ pointer: UnsafeMutablePointer<CChar>?) throws -> [String: Any] {
        guard let pointer else { throw protocolError("null RomeDB response") }
        defer { fdb_free(pointer) }
        let data = Data(String(cString: pointer).utf8)
        guard let response = try JSONSerialization.jsonObject(with: data) as? [String: Any],
              response["version"] as? Int == 1,
              let execution = response["execution"] as? [String: Any] else { throw protocolError("unsupported response") }
        if let error = execution["error"] as? [String: Any] {
            throw RomeDBError(diagnostic: error, transaction: response["transaction"] as? [String: Any])
        }
        return response
    }
    public init(path: String) throws {
        guard !path.utf8.contains(0), fdb_abi_version() == 1 else { throw Self.protocolError("invalid path or ABI") }
        let response = try path.withCString { try Self.receive(fdb_open($0)) }
        guard let execution = response["execution"] as? [String: Any],
              let text = execution["result"] as? String, let handle = UInt64(text) else { throw Self.protocolError("invalid handle") }
        self.handle = handle
    }
    /// Full protocol envelope, including transaction reports and lossless value tags.
    public func call(_ request: [String: Any], timeoutMs: Int64 = -1) throws -> [String: Any] {
        let data = try JSONSerialization.data(withJSONObject: request)
        let text = String(decoding: data, as: UTF8.self)
        return try text.withCString { try Self.receive(fdb_call(handle, $0, timeoutMs)) }
    }
    public func execute(_ sql: String, parameters: [String: Value] = [:], timeoutMs: Int64 = -1) throws -> [String: Any] {
        let response = try call(["op": "execute", "sql": sql, "parameters": parameters.mapValues(\.wire)], timeoutMs: timeoutMs)
        guard let execution = response["execution"] as? [String: Any], var result = execution["result"] as? [String: Any] else { throw Self.protocolError("invalid result") }
        result["transaction"] = response["transaction"]
        return result
    }
    public func interrupt() throws { _ = try Self.receive(fdb_interrupt(handle)) }
    public func close() throws { _ = try Self.receive(fdb_close(handle)) }
    deinit { fdb_free(fdb_close(handle)) }
}
