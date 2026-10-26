<?php
declare(strict_types=1);
namespace RomeDB;

final class Value implements \JsonSerializable {
    public function __construct(public readonly string $type, public readonly mixed $value = null) {}
    public function jsonSerialize(): array {
        return $this->type === 'Null' ? ['type' => 'Null'] : ['type' => $this->type, 'value' => $this->value];
    }
    public static function integer(int $n): self { return new self('Integer', (string)$n); }
    public static function string(string $s): self { return new self('String', $s); }
    public static function number(float $n): self {
        if (!is_finite($n)) throw new \InvalidArgumentException('number must be finite');
        return new self('Number', bin2hex(pack('E', $n)));
    }
    public static function boolean(bool $b): self { return new self('Boolean', $b); }
    public static function null(): self { return new self('Null'); }
    public static function array(array $v): self { return new self('Array', array_values($v)); }
    public static function object(array $v): self { return new self('Object', (object)$v); }
    public static function record(string $table, self $key): self { return new self('Record', ['table'=>$table, 'key'=>$key]); }
    public static function binary(string $bytes): self { return new self('Binary', array_values(unpack('C*', $bytes))); }
    public static function vector(string $bytes): self { return new self('Vector', array_values(unpack('C*', $bytes))); }
}
final class RomeDBException extends \RuntimeException {
    public function __construct(public readonly array $diagnostic, public readonly ?array $transaction) {
        parent::__construct($diagnostic['code'].': '.$diagnostic['message']);
    }
}
final class Database {
    private \FFI $ffi;
    private int $handle;
    public function __construct(string $path, string $library) {
        if (PHP_INT_SIZE !== 8) throw new \RuntimeException('RomeDB requires 64-bit PHP');
        if (str_contains($path, "\0")) throw new \InvalidArgumentException('path contains NUL');
        $this->ffi = \FFI::cdef('uint32_t fdb_abi_version(void); char *fdb_open(const char *); char *fdb_call(uint64_t, const char *, int64_t); char *fdb_close(uint64_t); char *fdb_interrupt(uint64_t); void fdb_free(char *);', $library);
        if ($this->ffi->fdb_abi_version() !== 1) throw new \RuntimeException('unsupported RomeDB ABI');
        $this->handle = (int)$this->receive($this->ffi->fdb_open($path))['execution']['result'];
    }
    private function receive(\FFI\CData $pointer): array {
        try { $r = json_decode(\FFI::string($pointer), true, 512, JSON_THROW_ON_ERROR); }
        finally { $this->ffi->fdb_free($pointer); }
        if ($r['version'] !== 1) throw new \RuntimeException('unsupported RomeDB response version');
        if (isset($r['execution']['error'])) throw new RomeDBException($r['execution']['error'], $r['transaction'] ?? null);
        return self::restoreObjects($r);
    }
    private static function restoreObjects(mixed $value): mixed {
        if (!is_array($value)) return $value;
        foreach ($value as $key=>$child) $value[$key] = self::restoreObjects($child);
        if (($value['type'] ?? null) === 'Object') $value['value'] = (object)$value['value'];
        return $value;
    }
    /** Full protocol response, including transaction state; values retain their lossless tags. */
    public function call(array $request, int $timeoutMs = -1): array {
        return $this->receive($this->ffi->fdb_call($this->handle, json_encode($request, JSON_THROW_ON_ERROR), $timeoutMs));
    }
    public function execute(string $sql, array $parameters = [], int $timeoutMs = -1): array {
        $r = $this->call(['op'=>'execute', 'sql'=>$sql, 'parameters'=>(object)$parameters], $timeoutMs);
        return $r['execution']['result'] + ['transaction'=>$r['transaction']];
    }
    public function interrupt(): void { $this->receive($this->ffi->fdb_interrupt($this->handle)); }
    public function close(): void { $this->receive($this->ffi->fdb_close($this->handle)); }
    public function __destruct() {
        if (isset($this->handle)) { try { $this->close(); } catch (\Throwable) {} }
    }
    private function __clone() {}
    public function __serialize(): array { throw new \LogicException('RomeDB handles cannot be serialized'); }
}
