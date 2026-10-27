#ifndef ROMEDB_H
#define ROMEDB_H
#include <stdint.h>
#ifdef __cplusplus
extern "C" {
#endif
/* ABI 1. UTF-8, NUL-terminated input is borrowed for the duration of each call.
 * Returned JSON strings belong to RomeDB; release exactly once with fdb_free.
 * Handles are process-local, never reused, and must not cross fork().
 * Calls on a handle serialize; callers must group multi-call transactions.
 * close waits for an active call and rolls back an uncommitted transaction.
 * timeout_ms is -1 (none) or >= 0, measured from entry including lock wait.
 * Response: {version:1,execution:{result:...}|{error:{code,message}},
 *            transaction?:{before,after}}. Integer/record/vector values use
 * RomeDB's transfer-v1 tagged encoding. open returns a decimal handle string.
 */
uint32_t fdb_abi_version(void);
char *fdb_open(const char *path);
char *fdb_call(uint64_t handle, const char *request, int64_t timeout_ms);
char *fdb_interrupt(uint64_t handle);
char *fdb_close(uint64_t handle);
void fdb_free(char *response);
#ifdef __cplusplus
}
#endif
#endif
