/*
 * Per-transaction metadata (ADR-0610): relate_conn_set_tx_metadata seeds
 * the connection's metadata map -- here {by: "app", run: 7} -- through
 * the same typed params builder that binds statement parameters; the
 * next transaction carries the map into its commit record, CALL
 * tx.getMetaData reads the live map back inside the transaction, and the
 * commit's boundary clears it. The machinery is the `value-provenance`
 * preview feature's (RELATE_PREVIEW, read once at open): a sample owns
 * its process, so it sets the variable in-process before the open -- a
 * library embedding Relate must not set the host's environment; the
 * process owner sets RELATE_PREVIEW=value-provenance before open, the
 * documented deployment path.
 *
 *   cc -I surfaces/relate-ffi/include example.c -L target/debug -lrelate_ffi
 */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include "relate.h"

static void take_error(const char *what, char *err) {
    fprintf(stderr, "%s failed: %s\n", what, err ? err : "(no detail)");
    relate_free_string(err);
}

/* One canonical statement-failure report: 1 when the result is an error. */
static int result_failed(relate_query_result r) {
    if (!relate_result_is_error(r)) return 0;
    fprintf(stderr, "statement failed: %s\n", relate_result_error_json(r));
    relate_destroy_result(r);
    return 1;
}

int main(void) {
#ifdef _WIN32
    _putenv("RELATE_PREVIEW=value-provenance");
#else
    setenv("RELATE_PREVIEW", "value-provenance", 1);
#endif

    relate_database db = NULL;
    relate_connection conn = NULL;
    char *err = NULL;

    if (relate_database_open(":memory:", &db, &err) != RELATE_OK) {
        take_error("database_open", err);
        return 1;
    }
    if (relate_connect_namespace(db, NULL, &conn, &err) != RELATE_OK) {
        take_error("connect_namespace", err);
        return 1;
    }

    /* The metadata map is a params builder: a root (or a put_map child),
     * read and not modified -- the same rule as relate_query_params's
     * params. */
    relate_params meta = relate_params_new();
    if (relate_params_put_string(meta, "by", "app", 3, &err) != RELATE_OK
            || relate_params_put_int(meta, "run", 7, &err) != RELATE_OK) {
        take_error("params_put", err);
        return 1;
    }
    if (relate_conn_set_tx_metadata(conn, meta, &err) != RELATE_OK) {
        take_error("conn_set_tx_metadata", err);
        return 1;
    }

    if (relate_conn_begin(conn, &err) != RELATE_OK) {
        take_error("begin", err);
        return 1;
    }
    relate_query_result write = NULL;
    if (relate_query_params(conn, "CREATE (n:Person {name: 'Ada'})", NULL, &write, &err)
        != RELATE_OK) {
        take_error("query_params", err);
        return 1;
    }
    relate_destroy_result(write);

    /* Read it back: the live map, inside the transaction that carries it.
     * The map reads through the walker like any result MAP. */
    relate_query_result live = NULL;
    if (relate_query_params(conn, "CALL tx.getMetaData() YIELD metadata RETURN metadata",
                            NULL, &live, &err) != RELATE_OK) {
        take_error("query_params", err);
        return 1;
    }
    if (result_failed(live)) return 1;
    relate_value map = relate_value_at(live, 0, 0);
    if (relate_value_kind(map) != RELATE_V_MAP || relate_value_map_len(map) != 2) {
        fprintf(stderr, "expected the seeded metadata map\n");
        return 1;
    }
    size_t by_len = 0;
    const char *by = relate_value_string(relate_value_map_get(map, "by"), &by_len);
    int64_t run = 0;
    if (by == NULL || by_len != 3 || memcmp(by, "app", 3) != 0
            || relate_value_int(relate_value_map_get(map, "run"), &run) != RELATE_OK
            || run != 7) {
        fprintf(stderr, "expected {by: 'app', run: 7}\n");
        return 1;
    }
    relate_destroy_result(live);

    err = NULL;
    char *bookmark = NULL;
    if (relate_conn_commit(conn, &bookmark, &err) != RELATE_OK) {
        take_error("commit", err);
        return 1;
    }
    printf("commit bookmark: %s\n", bookmark);
    relate_free_string(bookmark);

    /* Cleared after the boundary: the map rode the commit's record, so
     * nothing rides a later transaction. */
    relate_query_result after = NULL;
    if (relate_query_params(conn, "CALL tx.getMetaData() YIELD metadata RETURN metadata",
                            NULL, &after, &err) != RELATE_OK) {
        take_error("query_params", err);
        return 1;
    }
    if (result_failed(after)) return 1;
    if (relate_value_map_len(relate_value_at(after, 0, 0)) != 0) {
        fprintf(stderr, "expected an empty map after the commit\n");
        return 1;
    }
    relate_destroy_result(after);

    relate_params_free(meta);
    relate_disconnect(conn);
    relate_database_close(db);
    return 0;
}
