// Package romedb embeds RomeDB through its native C ABI. It requires cgo and
// libromedb_c on the linker and runtime library paths.
package romedb

/*
#cgo CFLAGS: -I${SRCDIR}/include
#cgo LDFLAGS: -lromedb_c
#include "romedb.h"
#include <stdlib.h>
*/
import "C"

import (
	"encoding/json"
	"errors"
	"fmt"
	"math"
	"reflect"
	"strconv"
	"strings"
	"unicode/utf8"
	"unsafe"
)

// Value is the lossless transfer-v1 value. Integer payloads are decimal strings;
// Number payloads are IEEE754 hex bits. Results keep these tags, including records.
type Value struct {
	Type  string `json:"type"`
	Value any    `json:"value"`
}

// Validate before encoding/json can replace malformed UTF-8 with U+FFFD.
func validText(v reflect.Value, depth int) error {
	if depth > 128 {
		return errors.New("request nesting exceeds 128")
	}
	if !v.IsValid() {
		return nil
	}
	switch v.Kind() {
	case reflect.Interface, reflect.Pointer:
		if !v.IsNil() {
			return validText(v.Elem(), depth+1)
		}
	case reflect.String:
		if !utf8.ValidString(v.String()) {
			return errors.New("text must be UTF-8")
		}
	case reflect.Slice, reflect.Array:
		for i := 0; i < v.Len(); i++ {
			if err := validText(v.Index(i), depth+1); err != nil {
				return err
			}
		}
	case reflect.Map:
		it := v.MapRange()
		for it.Next() {
			if err := validText(it.Key(), depth+1); err != nil {
				return err
			}
			if err := validText(it.Value(), depth+1); err != nil {
				return err
			}
		}
	case reflect.Struct:
		for i := 0; i < v.NumField(); i++ {
			if v.Type().Field(i).IsExported() {
				if err := validText(v.Field(i), depth+1); err != nil {
					return err
				}
			}
		}
	}
	return nil
}
func (v Value) MarshalJSON() ([]byte, error) {
	if v.Type == "Null" {
		return []byte(`{"type":"Null"}`), nil
	}
	type plain Value
	return json.Marshal(plain(v))
}
func Null() Value            { return Value{Type: "Null"} }
func Integer(v int64) Value  { return Value{"Integer", strconv.FormatInt(v, 10)} }
func String(v string) Value  { return Value{"String", v} }
func Boolean(v bool) Value   { return Value{"Boolean", v} }
func Number(v float64) Value { return Value{"Number", fmt.Sprintf("%016x", math.Float64bits(v))} }
func Array(v []Value) Value {
	if v == nil {
		v = []Value{}
	}
	return Value{"Array", v}
}
func Object(v map[string]Value) Value {
	if v == nil {
		v = map[string]Value{}
	}
	return Value{"Object", v}
}
func Record(table string, key Value) Value {
	return Value{"Record", map[string]any{"table": table, "key": key}}
}
func bytesValue(kind string, data []byte) Value {
	// []byte marshals as base64; portable values require an integer array.
	values := make([]int, len(data))
	for i, b := range data {
		values[i] = int(b)
	}
	return Value{kind, values}
}
func Binary(v []byte) Value { return bytesValue("Binary", v) }
func Vector(v []byte) Value { return bytesValue("Vector", v) }
func (v Value) Int64() (int64, error) {
	s, ok := v.Value.(string)
	if v.Type != "Integer" || !ok {
		return 0, errors.New("not an Integer")
	}
	return strconv.ParseInt(s, 10, 64)
}

type Transaction struct {
	Before string `json:"before"`
	After  string `json:"after"`
}
type Error struct {
	Code        string          `json:"code"`
	Message     string          `json:"message"`
	Migration   json.RawMessage `json:"migration,omitempty"`
	Transaction *Transaction    `json:"-"`
}

func (e *Error) Error() string { return e.Code + ": " + e.Message }

type Response struct {
	Version     int          `json:"version"`
	Transaction *Transaction `json:"transaction,omitempty"`
	Execution   struct {
		Result json.RawMessage `json:"result"`
		Error  *Error          `json:"error"`
	} `json:"execution"`
}
type Result struct {
	Columns     []string     `json:"columns"`
	Rows        [][]Value    `json:"rows"`
	Affected    int64        `json:"affected"`
	Transaction *Transaction `json:"-"`
}

// Database is an immutable process-local handle. Close is idempotent. Calls are
// serialized natively, but a multi-call transaction needs caller synchronization.
// Always defer Close; sharing copies does not duplicate the connection.
type Database struct{ handle C.uint64_t }

func response(p *C.char) (Response, error) {
	if p == nil {
		return Response{}, errors.New("null RomeDB response")
	}
	defer C.fdb_free(p)
	var r Response
	if err := json.Unmarshal([]byte(C.GoString(p)), &r); err != nil {
		return r, err
	}
	if r.Version != 1 {
		return r, errors.New("unsupported RomeDB response version")
	}
	if r.Execution.Error != nil {
		r.Execution.Error.Transaction = r.Transaction
		return r, r.Execution.Error
	}
	return r, nil
}
func Open(path string) (*Database, error) {
	if !utf8.ValidString(path) {
		return nil, errors.New("path must be UTF-8")
	}
	if strings.ContainsRune(path, 0) {
		return nil, errors.New("path contains NUL")
	}
	if C.fdb_abi_version() != 1 {
		return nil, errors.New("unsupported RomeDB ABI")
	}
	p := C.CString(path)
	defer C.free(unsafe.Pointer(p))
	r, err := response(C.fdb_open(p))
	if err != nil {
		return nil, err
	}
	var text string
	if err = json.Unmarshal(r.Execution.Result, &text); err != nil {
		return nil, err
	}
	handle, err := strconv.ParseUint(text, 10, 64)
	if err != nil {
		return nil, err
	}
	return &Database{C.uint64_t(handle)}, nil
}

// Call exposes every shared-protocol operation. timeoutMS is -1 or nonnegative.
func (db *Database) Call(request any, timeoutMS int64) (Response, error) {
	if err := validText(reflect.ValueOf(request), 0); err != nil {
		return Response{}, err
	}
	data, err := json.Marshal(request)
	if err != nil {
		return Response{}, err
	}
	p := C.CString(string(data))
	defer C.free(unsafe.Pointer(p))
	return response(C.fdb_call(db.handle, p, C.int64_t(timeoutMS)))
}
func (db *Database) Execute(sql string, parameters map[string]Value, timeoutMS int64) (Result, error) {
	if parameters == nil {
		parameters = map[string]Value{}
	}
	r, err := db.Call(map[string]any{"op": "execute", "sql": sql, "parameters": parameters}, timeoutMS)
	result := Result{Transaction: r.Transaction}
	if err != nil {
		return result, err
	}
	err = json.Unmarshal(r.Execution.Result, &result)
	return result, err
}
func (db *Database) Close() error     { _, err := response(C.fdb_close(db.handle)); return err }
func (db *Database) Interrupt() error { _, err := response(C.fdb_interrupt(db.handle)); return err }
