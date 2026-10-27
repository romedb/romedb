using RomeDB;
using System.Text.Json.Nodes;
static void Check(bool condition, string message) { if (!condition) throw new Exception(message); }
var path = Path.Combine(Path.GetTempPath(), "romedb-dotnet-" + Guid.NewGuid() + ".db");
var db = new Database(path);
try
{
    var r = db.Execute("SELECT $n,$b,$z,$nil,$a,$o", new JsonObject {
        ["$n"] = Value.Integer(long.MaxValue), ["$b"] = Value.Boolean(false),
        ["$z"] = Value.Number(-0.0), ["$nil"] = Value.Null(), ["$a"] = Value.Array(), ["$o"] = Value.Object(new())
    });
    Check(r["rows"]![0]![0]!["value"]!.GetValue<string>() == long.MaxValue.ToString(), "integer codec");
    Check(r["rows"]![0]![2]!["value"]!.GetValue<string>() == "8000000000000000", "negative zero");
    try { db.Execute("SELECT $s", new JsonObject { ["$s"] = Value.String("\ud800") }); throw new Exception("invalid Unicode accepted"); }
    catch (System.Text.EncoderFallbackException) { }
    var steps = JsonNode.Parse(File.ReadAllText(args[0]))!.AsArray();
    for (var i = 0; i < steps.Count; i++)
    {
        var step = steps[i]!;
        if (step["reopen"] is not null)
        {
            db.Dispose(); db.Dispose();
            try { db.Execute("SELECT 1"); throw new Exception("closed call succeeded"); }
            catch (RomeDBException e) { Check(e.Code == "FDB_VALIDATION", "closed code"); }
            db = new Database(path); continue;
        }
        try
        {
            var response = db.Call(step["request"]!.AsObject(), step["timeout_ms"]?.GetValue<long>() ?? -1);
            Check(step["error"] is null, $"step {i}: expected error");
            var result = response["execution"]!["result"]!;
            foreach (var field in new[] { "rows", "columns" })
                if (step[field] is not null) Check(JsonNode.DeepEquals(result[field], step[field]), $"step {i}: {field}: {result[field]}");
            if (step["result"] is not null) Check(JsonNode.DeepEquals(result, step["result"]), $"step {i}: result");
            if (step["result_subset"] is JsonObject subset)
                foreach (var pair in subset) Check(JsonNode.DeepEquals(result[pair.Key], pair.Value), $"step {i}: {pair.Key}");
        }
        catch (RomeDBException e)
        {
            Check(e.Code == step["error"]?.GetValue<string>(), $"step {i}: {e}");
            if (step["transaction"] is not null) Check(JsonNode.DeepEquals(e.Transaction, step["transaction"]), $"step {i}: transaction");
        }
    }
    Console.WriteLine($"{steps.Count} C# native contract steps passed");
}
finally
{
    db.Dispose();
    foreach (var file in Directory.GetFiles(Path.GetDirectoryName(path)!, Path.GetFileName(path) + "*")) File.Delete(file);
}
