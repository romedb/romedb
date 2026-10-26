using System.Runtime.InteropServices;
using System.Text.Json.Nodes;
using System.Globalization;

namespace RomeDB;

public sealed class RomeDBException(JsonObject diagnostic, JsonObject? transaction)
    : Exception($"{diagnostic["code"]}: {diagnostic["message"]}")
{
    public string Code => Diagnostic["code"]!.GetValue<string>();
    public JsonObject Diagnostic { get; } = diagnostic;
    public JsonObject? Transaction { get; } = transaction;
}

// Explicit typed values avoid decimal/double/int64 ambiguity across languages.
public static class Value
{
    private static JsonObject Tagged(string type, JsonNode? value) => new() { ["type"] = type, ["value"] = value };
    public static JsonObject Null() => new() { ["type"] = "Null" };
    public static JsonObject Integer(long value) => Tagged("Integer", JsonValue.Create(value.ToString(CultureInfo.InvariantCulture)));
    public static JsonObject String(string value) => Tagged("String", JsonValue.Create(value));
    public static JsonObject Boolean(bool value) => Tagged("Boolean", JsonValue.Create(value));
    public static JsonObject Number(double value)
    {
        if (!double.IsFinite(value)) throw new ArgumentOutOfRangeException(nameof(value));
        return Tagged("Number", JsonValue.Create(BitConverter.DoubleToUInt64Bits(value).ToString("x16")));
    }
    public static JsonObject Array(params JsonObject[] values) => Tagged("Array", new JsonArray(values.Select(v => (JsonNode)v.DeepClone()).ToArray()));
    public static JsonObject Object(JsonObject fields) => Tagged("Object", fields.DeepClone());
    public static JsonObject Record(string table, JsonObject key) => Tagged("Record", new JsonObject { ["table"] = table, ["key"] = key.DeepClone() });
    public static JsonObject Binary(byte[] data) => Bytes("Binary", data);
    public static JsonObject Vector(byte[] data) => Bytes("Vector", data);
    private static JsonObject Bytes(string type, byte[] data) => Tagged(type, new JsonArray(data.Select(b => (JsonNode)JsonValue.Create((int)b)!).ToArray()));
}

public sealed class Database : IDisposable
{
    private readonly ulong handle;
    private static void ValidateText(JsonNode? node, int depth = 0)
    {
        if (depth > 128) throw new ArgumentException("request nesting exceeds 128");
        var utf8 = new System.Text.UTF8Encoding(false, true);
        if (node is JsonValue value && value.TryGetValue<string>(out var text)) _ = utf8.GetByteCount(text);
        if (node is JsonObject fields)
            foreach (var pair in fields) { _ = utf8.GetByteCount(pair.Key); ValidateText(pair.Value, depth + 1); }
        if (node is JsonArray items)
            foreach (var item in items) ValidateText(item, depth + 1);
    }
    private static class Native
    {
        private const string Library = "romedb_c";
        [DllImport(Library, CallingConvention = CallingConvention.Cdecl)] internal static extern uint fdb_abi_version();
        [DllImport(Library, CallingConvention = CallingConvention.Cdecl)] internal static extern IntPtr fdb_open([MarshalAs(UnmanagedType.LPUTF8Str)] string path);
        [DllImport(Library, CallingConvention = CallingConvention.Cdecl)] internal static extern IntPtr fdb_call(ulong handle, [MarshalAs(UnmanagedType.LPUTF8Str)] string request, long timeoutMs);
        [DllImport(Library, CallingConvention = CallingConvention.Cdecl)] internal static extern IntPtr fdb_close(ulong handle);
        [DllImport(Library, CallingConvention = CallingConvention.Cdecl)] internal static extern IntPtr fdb_interrupt(ulong handle);
        [DllImport(Library, CallingConvention = CallingConvention.Cdecl)] internal static extern void fdb_free(IntPtr response);
    }
    private static JsonObject Receive(IntPtr pointer)
    {
        if (pointer == IntPtr.Zero) throw new InvalidOperationException("null RomeDB response");
        JsonObject response;
        try { response = JsonNode.Parse(Marshal.PtrToStringUTF8(pointer)!)!.AsObject(); }
        finally { Native.fdb_free(pointer); }
        if (response["version"]!.GetValue<int>() != 1) throw new InvalidOperationException("unsupported RomeDB response version");
        if (response["execution"]!["error"] is JsonObject error)
            throw new RomeDBException(error, response["transaction"] as JsonObject);
        return response;
    }
    public Database(string path)
    {
        ArgumentNullException.ThrowIfNull(path);
        _ = new System.Text.UTF8Encoding(false, true).GetByteCount(path);
        if (path.Contains('\0')) throw new ArgumentException("path contains NUL", nameof(path));
        if (Native.fdb_abi_version() != 1) throw new InvalidOperationException("unsupported RomeDB ABI");
        handle = ulong.Parse(Receive(Native.fdb_open(path))["execution"]!["result"]!.GetValue<string>(), CultureInfo.InvariantCulture);
    }
    /// <summary>All shared-protocol operations; results retain transfer-v1 tags. Timeout -1 means none.</summary>
    public JsonObject Call(JsonObject request, long timeoutMs = -1)
    {
        ValidateText(request);
        try { return Receive(Native.fdb_call(handle, request.ToJsonString(), timeoutMs)); }
        finally { GC.KeepAlive(this); }
    }
    public JsonObject Execute(string sql, JsonObject? parameters = null, long timeoutMs = -1)
    {
        var r = Call(new JsonObject { ["op"] = "execute", ["sql"] = sql, ["parameters"] = parameters?.DeepClone() ?? new JsonObject() }, timeoutMs);
        var result = r["execution"]!["result"]!.DeepClone().AsObject();
        result["transaction"] = r["transaction"]!.DeepClone();
        return result;
    }
    public void Interrupt() { try { Receive(Native.fdb_interrupt(handle)); } finally { GC.KeepAlive(this); } }
    public void Dispose() { Receive(Native.fdb_close(handle)); GC.SuppressFinalize(this); }
    ~Database() { if (handle != 0) { try { Native.fdb_free(Native.fdb_close(handle)); } catch { /* Finalizers must not throw. */ } } }
}
