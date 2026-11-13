/*
 * Namespace selection (ADR-0117): connect straight into a named fixed
 * namespace and switch the session namespace between transactions. A
 * statement routed somewhere it may not run is a statement failure: the
 * call returns RELATE_OK and the error rides the result.
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

/* Run one statement; NULL only for connection-level failures. */
static relate_query_result run(relate_connection conn, const char *query) {
    relate_query_result result = NULL;
    char *err = NULL;
    if (relate_query_params(conn, query, NULL, &result, &err) != RELATE_OK) {
        take_error("query_params", err); /* result is NULL */
    }
    return result;
}

int main(void) {
    /* ADR-0593: system_ontologies is visible only under the `ontology`
     * preview opt-in; set RELATE_PREVIEW before the open (it is read
     * once, at open). The portable spelling: MSVC has no setenv. */
#ifdef _WIN32
    _putenv_s("RELATE_PREVIEW", "ontology");
#else
    setenv("RELATE_PREVIEW", "ontology", 1);
#endif
    relate_database db = NULL;
    relate_connection conn = NULL;
    relate_query_result result = NULL;
    char *err = NULL;

    if (relate_database_open(":memory:", &db, &err) != RELATE_OK) {
        take_error("database_open", err);
        return 1;
    }
    /* Connect straight into system_ontologies; names match
     * case-insensitively. An unknown name is refused through the
     * out-param error channel: "Graph not found: <name>" (ADR-0146). The
     * namespace is visible under the `ontology` opt-in (ADR-0593) and not
     * queryable (ADR-0229): the one statement kind that still runs there
     * is the administration set (ADR-0116 §2). */
    if (relate_connect_namespace(db, "system_ontologies", &conn, &err) != RELATE_OK) {
        take_error("connect_namespace", err);
        return 1;
    }
    result = run(conn, "SHOW DATABASES");
    if (result == NULL) return 1;
    printf("databases=%lld\n", relate_row_count(result));
    relate_destroy_result(result);
    result = NULL;
    /* Every other statement routed there -- reads included -- fails with
     * 42N17's text (ADR-0229 §2). In v2 the failure rides the result:
     * the call itself returns RELATE_OK. */
    result = run(conn, "MATCH (n) RETURN count(n) AS n");
    if (result == NULL) return 1;
    if (!relate_result_is_error(result)) {
        fprintf(stderr, "expected the not-queryable error result\n");
        return 1;
    }
    printf("%s\n", relate_result_error_json(result));
    relate_destroy_result(result);
    result = NULL;

    /* Switch back to neo4j between transactions. */
    if (relate_conn_set_namespace(conn, "neo4j", &err) != RELATE_OK) {
        take_error("conn_set_namespace", err);
        return 1;
    }
    result = run(conn, "MATCH (n) RETURN count(n) AS n");
    if (result == NULL) return 1;
    if (relate_result_is_error(result) ||
        strcmp(relate_result_namespace(result), "neo4j") != 0) {
        fprintf(stderr, "expected a good neo4j result\n");
        return 1;
    }
    printf("back on %s\n", relate_result_namespace(result));
    relate_destroy_result(result);

    relate_disconnect(conn);
    relate_database_close(db);
    return 0;
}
