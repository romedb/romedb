<?php
declare(strict_types=1);
require __DIR__.'/../src/RomeDB.php';
use RomeDB\Database;
use RomeDB\RomeDBException;
use RomeDB\Value;
function check(bool $condition, string $message): void {
    if (!$condition) throw new RuntimeException($message);
}
$path = sys_get_temp_dir().'/romedb-php-'.bin2hex(random_bytes(8)).'.db';
$library = getenv('ROMEDB_LIBRARY');
$db = new Database($path, $library);
try {
    $r = $db->execute('SELECT $n,$s,$b,$z,$o,$a,$nil', [
        '$n'=>Value::integer(PHP_INT_MAX), '$s'=>Value::string('ภาษาไทย'),
        '$b'=>Value::boolean(false), '$z'=>Value::number(-0.0),
        '$o'=>Value::object([]), '$a'=>Value::array([]), '$nil'=>Value::null()]);
    check($r['rows'][0][0]['value'] === (string)PHP_INT_MAX, 'integer codec');
    check($r['rows'][0][3]['value'] === '8000000000000000', 'negative zero');
    check(is_object($r['rows'][0][4]['value']), 'empty object preservation');
    $db->execute('SELECT $o', ['$o'=>$r['rows'][0][4]]);
    // Decode as objects to preserve empty JSON object versus empty array in inputs.
    $fixture = getenv('ROMEDB_FIXTURE') ?: (is_file(__DIR__.'/../testdata/native-client.json') ? __DIR__.'/../testdata/native-client.json' : __DIR__.'/../../fixtures/native-client.json');
    $steps = json_decode(file_get_contents($fixture), false, 512, JSON_THROW_ON_ERROR);
    foreach ($steps as $index=>$step) {
        if (isset($step->reopen)) {
            $db->close(); $db->close();
            try { $db->execute('SELECT 1'); throw new RuntimeException('closed call succeeded'); }
            catch (RomeDBException $e) { check($e->diagnostic['code'] === 'FDB_VALIDATION', 'closed code'); }
            $db = new Database($path, $library); continue;
        }
        try {
            $response = $db->call((array)$step->request, $step->timeout_ms ?? -1);
            check(!isset($step->error), "step $index expected error");
            $result = $response['execution']['result'];
            foreach (['rows','columns'] as $field) {
                if (isset($step->$field)) check(json_decode(json_encode($result[$field]), true) == json_decode(json_encode($step->$field), true), "step $index $field");
            }
            if (isset($step->result)) check($result === $step->result, "step $index result");
            if (isset($step->result_subset)) foreach ($step->result_subset as $k=>$v) check($result[$k] === $v, "step $index $k");
        } catch (RomeDBException $e) {
            check(isset($step->error) && $e->diagnostic['code'] === $step->error, "step $index: ".$e->getMessage());
            if (isset($step->transaction)) check($e->transaction == (array)$step->transaction, "step $index transaction");
        }
    }
    echo count($steps)." PHP native contract steps passed\n";
} finally {
    $db->close();
    foreach (glob($path.'*') as $file) unlink($file);
}
