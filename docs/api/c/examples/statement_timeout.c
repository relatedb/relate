/*
 * Statement timeout (ADR-0692): bound each statement on a connection. A
 * statement still running past its budget is cancelled; the call returns
 * RELATE_OK and the timeout rides the result as an error result (a
 * statement failure, ADR-0502 §2). The connection stays usable, and 0
 * clears the bound.
 *
 *   cc -I surfaces/relate-ffi/include example.c -L target/debug -lrelate_ffi
 */
#include <stdio.h>
#include <string.h>
#include "relate.h"

static void take_error(const char *what, char *err) {
    fprintf(stderr, "%s failed: %s\n", what, err ? err : "(no detail)");
    relate_free_string(err);
}

int main(void) {
    relate_database db = NULL;
    relate_connection conn = NULL;
    relate_query_result result = NULL;
    char *err = NULL;

    if (relate_database_open(":memory:", &db, &err) != RELATE_OK) {
        take_error("database_open", err);
        return 1;
    }
    if (relate_connect_namespace(db, "neo4j", &conn, &err) != RELATE_OK) {
        take_error("connect_namespace", err);
        return 1;
    }

    /* 200 ms per statement. */
    if (relate_conn_set_statement_timeout(conn, 200, &err) != RELATE_OK) {
        take_error("conn_set_statement_timeout", err);
        return 1;
    }
    if (relate_query_params(conn,
                            "UNWIND range(1, 2000000000) AS x RETURN count(x) AS n",
                            NULL, &result, &err) != RELATE_OK) {
        take_error("query_params", err);
        return 1;
    }
    if (!relate_result_is_error(result)) {
        fprintf(stderr, "expected the long statement to time out\n");
        return 1;
    }
    /* The error shape carries the Neo4j status code and the message. */
    printf("timed out: %s\n", relate_result_error_json(result));
    relate_destroy_result(result);

    /* 0 clears the bound; the connection is still usable. */
    if (relate_conn_set_statement_timeout(conn, 0, &err) != RELATE_OK) {
        take_error("conn_set_statement_timeout", err);
        return 1;
    }
    if (relate_query_params(conn, "RETURN 1 AS n", NULL, &result, &err) != RELATE_OK) {
        take_error("query_params", err);
        return 1;
    }
    printf("rows=%lld\n", relate_row_count(result));
    relate_destroy_result(result);

    relate_disconnect(conn);
    relate_database_close(db);
    return 0;
}
