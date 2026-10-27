package romedb

import (
	"encoding/json"
	"errors"
	"math"
	"os"
	"path/filepath"
	"reflect"
	"testing"
)

func TestNativeContract(t *testing.T) {
	path := filepath.Join(t.TempDir(), "native.db")
	db, err := Open(path)
	if err != nil {
		t.Fatal(err)
	}
	defer func() { _ = db.Close() }()
	typed, err := db.Execute("SELECT $n,$b,$z,$a,$o,$nil", map[string]Value{
		"$n": Integer(9223372036854775807), "$b": Boolean(false), "$z": Number(math.Copysign(0, -1)), "$a": Array(nil), "$o": Object(nil), "$nil": Null(),
	}, -1)
	if err != nil {
		t.Fatal(err)
	}
	n, err := typed.Rows[0][0].Int64()
	if err != nil || n != 9223372036854775807 {
		t.Fatal("integer codec", n, err)
	}
	if typed.Rows[0][2].Value != "8000000000000000" {
		t.Fatal("negative zero")
	}
	if _, err = db.Execute("SELECT $s", map[string]Value{"$s": String("\xff")}, -1); err == nil {
		t.Fatal("invalid UTF-8 accepted")
	}
	var steps []struct {
		Reopen      bool                       `json:"reopen"`
		Request     json.RawMessage            `json:"request"`
		Timeout     *int64                     `json:"timeout_ms"`
		Error       string                     `json:"error"`
		Transaction *Transaction               `json:"transaction"`
		Rows        json.RawMessage            `json:"rows"`
		Columns     json.RawMessage            `json:"columns"`
		Result      json.RawMessage            `json:"result"`
		Subset      map[string]json.RawMessage `json:"result_subset"`
	}
	fixture := os.Getenv("ROMEDB_FIXTURE")
	if fixture == "" {
		fixture = "testdata/native-client.json"
		if _, err := os.Stat(fixture); err != nil {
			fixture = "../fixtures/native-client.json"
		}
	}
	data, err := os.ReadFile(fixture)
	if err != nil {
		t.Fatal(err)
	}
	if err = json.Unmarshal(data, &steps); err != nil {
		t.Fatal(err)
	}
	same := func(a, b json.RawMessage) bool {
		var x, y any
		if json.Unmarshal(a, &x) != nil || json.Unmarshal(b, &y) != nil {
			return false
		}
		return reflect.DeepEqual(x, y)
	}
	for i, s := range steps {
		if s.Reopen {
			if err = db.Close(); err != nil {
				t.Fatal(err)
			}
			if err = db.Close(); err != nil {
				t.Fatal(err)
			}
			if _, err = db.Execute("SELECT 1", nil, -1); err == nil {
				t.Fatal("closed call succeeded")
			}
			db, err = Open(path)
			if err != nil {
				t.Fatal(err)
			}
			continue
		}
		timeout := int64(-1)
		if s.Timeout != nil {
			timeout = *s.Timeout
		}
		r, err := db.Call(s.Request, timeout)
		if s.Error != "" {
			var e *Error
			if !errors.As(err, &e) || e.Code != s.Error {
				t.Fatalf("step %d: %v", i, err)
			}
			if s.Transaction != nil && !reflect.DeepEqual(e.Transaction, s.Transaction) {
				t.Fatalf("step %d transaction", i)
			}
			continue
		}
		if err != nil {
			t.Fatalf("step %d: %v", i, err)
		}
		if s.Result != nil && !same(r.Execution.Result, s.Result) {
			t.Fatalf("step %d result", i)
		}
		if s.Rows != nil || s.Columns != nil || s.Subset != nil {
			var result map[string]json.RawMessage
			if err = json.Unmarshal(r.Execution.Result, &result); err != nil {
				t.Fatal(err)
			}
			for key, want := range map[string]json.RawMessage{"rows": s.Rows, "columns": s.Columns} {
				if want != nil && !same(result[key], want) {
					t.Fatalf("step %d %s: %s", i, key, result[key])
				}
			}
			for key, want := range s.Subset {
				if !same(result[key], want) {
					t.Fatalf("step %d %s", i, key)
				}
			}
		}
	}
	t.Logf("%d native contract steps passed", len(steps))
}
