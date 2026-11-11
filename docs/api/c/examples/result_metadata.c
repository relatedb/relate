/*
 * Result metadata (ADR-0285): a result's query type, resolved namespace,
 * mutation counters and engine-reported timings. Every string is owned by
 * the result and valid until relate_destroy_result() -- do not free
 * them. Per-value types are read from the value walker's kind.
 *
 *   cc -I surfaces/relate-ffi/include example.c -L target/debug -lrelate_ffi
 */
#include <stdio.h>
#include "relate.h"

static void take_error(const char *what, char *err) {
    fprintf(stderr, "%s failed: %s\n", what, err ? err : "(no detail)");
    relate_free_string(err);
}

/* Run one statement, refusing both failure modes — the query /
 * take_error / return ladder, once. A transport-level failure is a
 * RELATE_ERROR status; a statement failure rides the result
 * (RELATE_OK with is_error 1). Either way the caller sees one
 * refusal (NULL), not a half-run example; on success the caller owns
 * the returned result. */
static relate_query_result run(relate_connection conn, const char *cypher) {
    relate_query_result out = NULL;
    char *err = NULL;
    if (relate_query_params(conn, cypher, NULL, &out, &err) != RELATE_OK) {
        take_error("query_params", err);
        return NULL;
    }
    if (relate_result_is_error(out)) {
        fprintf(stderr, "statement failed: %s\n", relate_result_error_json(out));
        relate_destroy_result(out);
        return NULL;
    }
    return out;
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
    if (relate_connect_namespace(db, NULL, &conn, &err) != RELATE_OK) {
        take_error("connect_namespace", err);
        return 1;
    }

    /* Query type (ADR-0162, ADR-0332): "r" (read), "w" (a write with no
     * returned columns), "rw" (a write with returned columns) or "s"
     * (schema), classified from the plan. EXPLAIN reports the wrapped
     * statement's type without executing it, so this prints "w" and no
     * node is created. */
    result = run(conn, "EXPLAIN CREATE (n)");
    if (!result) return 1;
    printf("query_type=%s\n", relate_result_query_type(result));
    relate_destroy_result(result);
    result = NULL;

    /* Resolved namespace: the USE target when the statement carries one,
     * otherwise the session namespace. This prints "system". A USE clause
     * routes one statement only; it never changes the session namespace. */
    result = run(conn, "USE system SHOW DATABASES YIELD name RETURN name");
    if (!result) return 1;
    printf("namespace=%s\n", relate_result_namespace(result));
    relate_destroy_result(result);
    result = NULL;

    /* The session namespace is still neo4j: an unrouted statement resolves
     * to it, printing "neo4j" / "r". */
    result = run(conn, "MATCH (n) RETURN count(n) AS n");
    if (!result) return 1;
    printf("namespace=%s query_type=%s\n",
           relate_result_namespace(result),
           relate_result_query_type(result));
    relate_destroy_result(result);
    result = NULL;

    /* Mutation counters (ADR-0216): a JSON object string with the canonical
     * 11, snake_case, zeros included. This prints the counters object for a
     * one-node CREATE. Same ownership rule: result-owned, do not free. */
    result = run(conn, "CREATE (n:Person {name: 'Alice'})");
    if (!result) return 1;
    printf("stats=%s\n", relate_result_stats(result));
    relate_destroy_result(result);
    result = NULL;

    /* Per-value types come from the walker's kind, structural at every
     * depth: this prints name=STRING age=INTEGER node=NODE. An out-of-range
     * cell has no handle at all (relate_value_at returns NULL). */
    result = run(conn, "CREATE (n:Person {name: 'Bob', age: 42}) "
                "RETURN n.name AS name, n.age AS age, n AS node");
    if (!result) return 1;
    printf("name=%d age=%d node=%d\n",
           relate_value_kind(relate_value_at(result, 0, 0)),
           relate_value_kind(relate_value_at(result, 0, 1)),
           relate_value_kind(relate_value_at(result, 0, 2)));
    if (relate_value_kind(relate_value_at(result, 0, 0)) != RELATE_V_STRING ||
        relate_value_kind(relate_value_at(result, 0, 1)) != RELATE_V_INT ||
        relate_value_kind(relate_value_at(result, 0, 2)) != RELATE_V_NODE) {
        fprintf(stderr, "expected STRING / INT / NODE kinds\n");
        return 1;
    }
    relate_destroy_result(result);
    result = NULL;

    /* Engine-reported elapsed (ADR-0221): wall-clock milliseconds for the
     * statement's parse, plan and run, stamped by the engine -- not
     * measured at this client edge. The phase split (ADR-0491) breaks that
     * total into its PARSE / PLAN / EXECUTE windows: disjoint sub-windows
     * whose sum never exceeds the elapsed. */
    result = run(conn, "RETURN 1 AS n");
    if (!result) return 1;
    printf("elapsed_ms=%.3f (parse %.3f / plan %.3f / execute %.3f)\n",
           relate_result_elapsed_ms(result),
           relate_result_parse_ms(result),
           relate_result_plan_ms(result),
           relate_result_execute_ms(result));
    relate_destroy_result(result);

    relate_disconnect(conn);
    relate_database_close(db);
    return 0;
}
