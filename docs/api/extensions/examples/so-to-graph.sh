#!/usr/bin/env bash
# so-to-graph — the hello-world MCP extension in bash: Stack Overflow questions
# into a graph, with curl and jq and nothing else.
#
# An extension is the MCP client: the host writes the request envelope (the
# resolved --tag/--pages/--body values, one JSON line) on the child's stdin,
# and from then on the child writes JSON-RPC 2.0 requests on stdout and reads
# the host's responses on stdin. This child drives the host's tools
# (update_schema, batch_cypher, write_cypher, read_cypher, report_output) and
# the host alone owns the store.
#
# The corpus is live: the public Stack Exchange API's newest questions for one
# tag, fetched with the corpus's standard filter — answers, owners, tags
# and body_markdown embedded. --pages N walks
# API pages 1..N, each a fresh request (the API's page size is fixed at 100).
# The schema and the load are that notebook's sections 2-3: an SOGraph graph
# type declared and promoted through update_schema, then the API's rows as
# (:Question), (:Answer), (:User) nodes with ASKED, TAGGED, ANSWERED and
# PROVIDED edges. --body writes the API's own body_markdown onto the question
# and answer nodes (declared in the graph type).
#
# Data: Stack Overflow through the Stack Exchange API
# (https://api.stackexchange.com/docs/questions); Stack Overflow content is
# licensed CC BY-SA 4.0 (https://creativecommons.org/licenses/by-sa/4.0/), and
# this copy carries the same license. A question's id links back to
# https://stackoverflow.com/q/<id>. Needs network access, and a fresh store
# (the promoted graph type refuses a re-import of the same questions).
#
# Flags (declared in extension.bash.json, resolved by the host into the request
# envelope): --tag (string, default neo4j), --pages (integer, default 1),
# --body (boolean).
#
#   relate run so-to-graph --tag neo4j --pages 1 --body -d ./so
#
# The JSON-RPC plumbing is `mcp-client.sh`, inlined verbatim at the bottom of
# this file because a package is exactly two files — the manifest and the
# executable (`extension install --from` copies nothing else).

# --- the example: Stack Overflow's newest questions walked into a graph ------
#
# Read top to bottom: the flags, the graph type, the two load statements, the
# duplicate-key check, the summary read, and the report. The JSON-RPC plumbing
# those calls need is `mcp-client.sh`, inlined verbatim at the bottom of this
# file (a package is one executable). Runtime: bash, `curl` and `jq`.
main() {
    mcp_init so-to-graph 0.1.0
    tag=$(mcp_flag_string tag)
    pages=$(mcp_flag_int pages)
    body=$(mcp_flag_bool body)
    [ -n "$tag" ] || tag=neo4j
    [ "$pages" -gt 0 ] || pages=1

    # 1. The corpus: the Stack Exchange API's newest questions for the tag,
    # pages 1..N, the way the cookbook notebook's generator fetches them
    # (`--compressed` decodes the API's gzip body).
    questions=$(fetch_questions "$tag" "$pages")
    question_count=$(printf '%s' "$questions" | jq 'length')
    [ "$question_count" -gt 0 ] ||
        mcp_fail "the API returned no questions for '$tag'"
    answers=$(printf '%s' "$questions" | jq -c '[.[].answers // [] | .[]]')

    # 2. The generated notebook's graph type (its section 3), then its
    # promotion, which derives the enforcing constraints.
    mcp_call update_schema "$(jq -cn --arg s "$GRAPH_TYPE" '{statement: $s}')" >/dev/null
    mcp_call update_schema "$(jq -cn --arg s "$PROMOTE" '{statement: $s}')" >/dev/null

    # 3. The load, one call: the two statements of the notebook's pass —
    # questions (with their tags and askers), then the answers (with their
    # answerers) — in the one transaction `batch_cypher` gives the call.
    # The placeholder sits where the optional `--body` clause goes, and both
    # spellings replace it — the clause, or nothing — exactly as the Python
    # and TypeScript examples do: a run without `--body` must read
    # `r.link`/`value.score`, never the placeholder glued to the identifier.
    body_question=
    body_answer=
    if [ "$body" = true ]; then
        body_question=", q.body_markdown = r.body_markdown"
        body_answer=", a.body_markdown = value.body_markdown"
    fi
    substitute_statement questions_statement "$QUESTIONS" BODY_QUESTION "$body_question"
    substitute_statement answers_statement "$ANSWERS" BODY_ANSWER "$body_answer"
    load=$(jq -cn --arg q "$questions_statement" --arg a "$answers_statement" \
        --argjson qr "$questions" --argjson ar "$answers" \
        '{statements: [{statement: $q, parameters: {rows: $qr}},
                       {statement: $a, parameters: {rows: $ar}}]}')
    mcp_call batch_cypher "$load" >/dev/null

    # 4. The promotion's derived key constraint is what the schema step
    # asserts (a graph type is no index or constraint row): a duplicate
    # question key must now be refused. The request is raw because a refusal
    # is the expected answer, which `mcp_call` treats as a failure.
    qid=$(printf '%s' "$questions" | jq -r '.[0].question_id')
    refuse_duplicate "$qid"

    # 5. The notebook's own summary read: what actually landed.
    found=$(mcp_call read_cypher "$(jq -cn --arg s "$VERIFY" '{statement: $s}')")
    users=$(printf '%s' "$questions" | jq '[.[].owner.user_id]')
    answers_users=$(printf '%s' "$answers" | jq '[.[].owner.user_id]')
    expected_tags=$(printf '%s' "$questions" | jq '[.[].tags[]] | unique | length')
    expected_users=$(jq -cn --argjson a "$users" --argjson b "$answers_users" '$a + $b | unique | length')
    [ "$(mcp_number "$found" questions)" = "$question_count" ] ||
        mcp_fail "read_cypher found $(mcp_number "$found" questions) questions, expected $question_count"
    [ "$(mcp_number "$found" answers)" = "$(printf '%s' "$answers" | jq 'length')" ] ||
        mcp_fail "read_cypher found $(mcp_number "$found" answers) answers, expected $(printf '%s' "$answers" | jq 'length')"
    [ "$(mcp_number "$found" tags)" = "$expected_tags" ] ||
        mcp_fail "read_cypher found $(mcp_number "$found" tags) tags, expected $expected_tags"
    [ "$(mcp_number "$found" users)" = "$expected_users" ] ||
        mcp_fail "read_cypher found $(mcp_number "$found" users) users, expected $expected_users"

    # 6. The summary: the counts, then the notebook's two starter queries.
    note=
    [ "$body" = true ] && note=" with bodies"
    text="imported $(mcp_number "$found" questions) questions, $(mcp_number "$found" answers) answers, $(mcp_number "$found" tags) tags and $(mcp_number "$found" users) users tagged '$tag'$note"
    report=$(jq -cn --arg t "$text" --argjson s "$STARTERS" '{text: $t, starters: $s}')
    mcp_call report_output "$report" >/dev/null
}

# substitute_statement OUT STATEMENT PLACEHOLDER CLAUSE — STATEMENT with
# PLACEHOLDER replaced by CLAUSE (deleted when CLAUSE is empty), into OUT.
# Bash's substitution is a silent no-op when the pattern is absent, so the
# placeholder the statement constants carry is checked here: a constant
# that lost it fails loudly instead of importing a statement that quietly
# omits the clause.
substitute_statement() {
    case $2 in
        *"$3"*) printf -v "$1" '%s' "${2/"$3"/$4}" ;;
        *) mcp_fail "$3 is missing from the statement it belongs to" ;;
    esac
}

# fetch_questions TAG PAGES — the API's newest questions for TAG, pages 1..PAGES,
# accumulated into one JSON array (the page size is fixed at 100).
fetch_questions() {
    local tag=$1 pages=$2 page=1 collected='[]' payload more
    while [ "$page" -le "$pages" ]; do
        payload=$(curl -sS --compressed --max-time 30 \
            "https://api.stackexchange.com/2.2/questions?pagesize=100&order=desc&sort=creation&page=$page&tagged=$tag&site=stackoverflow&filter=!5-i6Zw8Y)4W7vpy91PMYsKM-k9yzEsSC1_Uxlf") ||
            mcp_fail "the API request for '$tag' page $page failed"
        collected=$(jq -c --argjson acc "$collected" '.items as $items | $acc + $items' <<<"$payload")
        more=$(jq -r '.has_more // false' <<<"$payload")
        [ "$more" = true ] || break
        page=$((page + 1))
    done
    printf '%s' "$collected"
}

# refuse_duplicate QID — attempt a second Question with QID's key and fail the
# child unless the host refuses it.
refuse_duplicate() {
    local qid=$1 arguments response
    arguments=$(jq -cn --argjson u "$qid" \
        '{statement: "CREATE (q:Question {uuid: $uuid, score: 0})", parameters: {uuid: $u}}')
    MCP_ID=$((MCP_ID + 1))
    printf '{"jsonrpc":"2.0","id":%d,"method":"tools/call","params":{"name":"write_cypher","arguments":%s}}\n' \
        "$MCP_ID" "$arguments" >&3
    IFS= read -r response || mcp_fail "the host closed the session before the duplicate-key answer"
    case $response in
        *'"error"'*) ;;
        *) mcp_fail "the promoted graph type admitted a duplicate key: $response" ;;
    esac
}

# ---------------------------------------------------------------------------
# The data and the statements.
# ---------------------------------------------------------------------------

# The corpus's graph type: questions, answers, users and tags with their typed
# properties and the four relationship types. `body_markdown` is declared
# because `--body` writes it, so the schema and the data agree.
read -r -d '' GRAPH_TYPE <<'CYPHER'
CREATE GRAPH TYPE SOGraph AS {
  (:Question => {uuid :: INTEGER NOT NULL IS KEY, score :: INTEGER,
                 view_count :: INTEGER, answer_count :: INTEGER,
                 is_answered :: BOOLEAN, link :: STRING,
                 body_markdown :: STRING}),
  (:Answer => {uuid :: INTEGER NOT NULL IS KEY, score :: INTEGER,
               is_accepted :: BOOLEAN, link :: STRING, body_markdown :: STRING}),
  (:User => {display_name :: STRING, reputation :: INTEGER,
             user_type :: STRING, link :: STRING}),
  (:Tag => {name :: STRING NOT NULL IS UNIQUE}),
  (:User)-[:ASKED]->(:Question),
  (:Question)-[:TAGGED]->(:Tag),
  (:Answer)-[:ANSWERED]->(:Question),
  (:User)-[:PROVIDED]->(:Answer)
}
CYPHER

PROMOTE='ALTER CURRENT GRAPH TYPE SET GRAPH TYPE SOGraph'

# The questions load, one statement for the whole row set — the reference
# pattern (its `apoc.load.json` becomes the tool's `$rows` parameter, which is
# what replaced the retired wire's batch/rows records).
read -r -d '' QUESTIONS <<'CYPHER'
UNWIND $rows AS r
MERGE (q:Question {uuid: r.question_id})
ON CREATE SET q.score = r.score, q.view_count = r.view_count,
              q.answer_count = r.answer_count, q.is_answered = r.is_answered,
              q.link = r.linkBODY_QUESTION
FOREACH (name IN r.tags |
  MERGE (t:Tag {name: name})
  CREATE (q)-[:TAGGED]->(t))
MERGE (asker:User {uuid: r.owner.user_id})
ON CREATE SET asker.display_name = r.owner.display_name,
              asker.reputation = r.owner.reputation,
              asker.user_type = r.owner.user_type, asker.link = r.owner.link
MERGE (asker)-[:ASKED]->(q)
CYPHER

# The answers, the reference statement's shape: MERGE the Answer, MERGE its
# Question, MERGE the ANSWERED edge, then MERGE the answerer and the PROVIDED
# edge, returning the count.
read -r -d '' ANSWERS <<'CYPHER'
UNWIND $rows AS value
MERGE (a:Answer {uuid: value.answer_id})
ON CREATE SET a.is_accepted = value.is_accepted, a.link = value.link,
              a.score = value.scoreBODY_ANSWER
MERGE (q:Question {uuid: value.question_id})
MERGE (a)-[:ANSWERED]->(q)
WITH a AS answer, value.owner AS owner
MERGE (u:User {uuid: owner.user_id})
ON CREATE SET u.display_name = owner.display_name,
              u.reputation = owner.reputation,
              u.user_type = owner.user_type, u.link = owner.link
MERGE (u)-[:PROVIDED]->(answer)
RETURN count(answer) AS answers
CYPHER

# The notebook's own summary read: questions, answers, tags and users.
read -r -d '' VERIFY <<'CYPHER'
MATCH (q:Question)
WITH count(q) AS questions
RETURN questions, COUNT { (:Answer) } AS answers, COUNT { (:Tag) } AS tags,
       COUNT { (:User) } AS users
CYPHER

# The notebook's starter queries: tag co-occurrence, and who answers the tag's
# questions.
STARTERS=$(jq -cn '[
  "MATCH (t1:Tag)<-[:TAGGED]-(q:Question)-[:TAGGED]->(t2:Tag)\nWHERE t1.name < t2.name\nRETURN t1.name AS tag, t2.name AS co_occurs, count(q) AS questions\nORDER BY questions DESC, tag, co_occurs LIMIT 10",
  "MATCH (u:User)-[:PROVIDED]->(:Answer)-[:ANSWERED]->(:Question)-[:TAGGED]->(t:Tag)\nRETURN u.display_name AS user, count(*) AS answers\nORDER BY answers DESC, user LIMIT 10"
]')

# ---------------------------------------------------------------------------
# Boilerplate: the mount's plumbing (the inlined `mcp-client.sh`). Nothing
# below says what this extension DOES.
# ---------------------------------------------------------------------------
# >>> mcp-client.sh (verbatim; scripts/check-extension-examples.sh pins it) >>>
# The MCP revision this kit speaks — the wire kit's own constant
# (`relate_wire::mcp::MCP_PROTOCOL_VERSION`). The host's flag values are
# what it acts on; the revision is the client's half of the handshake, and
# `mcp_init` fails a host that answers another one instead of downgrading.
MCP_PROTOCOL_VERSION=${MCP_PROTOCOL_VERSION:-2025-06-18}

# The host's request envelope, once `mcp_init` read it.
MCP_ENVELOPE=
# The last correlation id sent; the handshake takes 1.
MCP_ID=0
# The initialize result, once `mcp_init` read it.
MCP_INIT_RESULT=
# The host's request pipe (fd 3, set by `mcp_init`): requests go here, so a
# `$(mcp_call …)` capture takes the tool result and not the request.
MCP_FD=

mcp_fail() {
    printf 'mcp: %s\n' "$*" >&2
    exit 1
}

# The raw envelope text from flag NAME's value on — everything after
# `"NAME":`, exactly as the envelope spells it. The one place the
# envelope is searched for a flag: `mcp_fail` when the manifest and the
# child disagree about the flag table. A string flag's slice goes to the
# kit's one decoder, which terminates it at the value's own closing
# quote (the `mcp_string` shape for tool results), so a string is
# decoded exactly once — never decoded, re-encoded by `json_string`,
# and decoded again.
_mcp_flag_slice() {
    case $MCP_ENVELOPE in
        *"\"$1\":"*) printf '%s' "${MCP_ENVELOPE#*\"$1\":}" ;;
        *) mcp_fail "the request envelope carries no '$1' flag" ;;
    esac
}

# The scalar token of flag NAME (`2`, `false`), shaped for the scalar
# readers (`mcp_flag_int`, `mcp_flag_bool`). A string flag is the string
# reader's and a list the list reader's — this reader never decodes,
# re-encodes or brackets anything. Fails when the manifest and the child
# disagree about the flag table.
_mcp_flag_value() {
    local rest
    rest=$(_mcp_flag_slice "$1")
    # A scalar ends at the next `,` — or at the envelope's closing `}`
    # when the flag is the last one. The two cuts stay separate because
    # `}` inside a bracket expression ends the expansion.
    rest=${rest%%,*}
    printf '%s' "${rest%%\}*}"
}

# _mcp_decode_string TOKEN OUT [TAIL] — the value of the JSON string
# literal TOKEN (its opening quote included), escapes undone, into the
# variable OUT; the text after the closing quote, into TAIL when given
# (the list walker needs it). Returns 1 — no message, no exit, so it
# works wherever it is called, command substitutions included — when
# TOKEN is not a string, its closing quote never comes (an unterminated
# value is a failure, never a silent truncation), or a `\u` escape's
# four hex digits are missing. The message and the exit belong to the
# caller, which knows the flag or tool the value belongs to.
_mcp_decode_string() {
    # The out-parameter names are held in dedicated locals (`out_name`,
    # `tail_name`): a local sharing a caller's variable name would make
    # `printf -v` write the local, not the caller's variable.
    local rest=$1 out_name=$2 tail_name=${3-} value= char hexchar
    case $rest in
        \"*) rest=${rest#\"} ;;
        *) return 1 ;;
    esac
    while [ -n "$rest" ]; do
        char=${rest:0:1}
        rest=${rest:1}
        # A backslash escapes the next character: the JSON shortcuts,
        # the quote/backslash themselves, and \uHHHH for the rest of the
        # control characters — decoded by the one predicate, so a short
        # or non-hex escape fails like every other malformed token.
        if [ "$char" = '\' ]; then
            char=${rest:0:1}
            rest=${rest:1}
            case $char in
                n) value+=$'\n' ;;
                t) value+=$'\t' ;;
                r) value+=$'\r' ;;
                b) value+=$'\b' ;;
                f) value+=$'\f' ;;
                u)
                    case $rest in
                        [0-9a-fA-F][0-9a-fA-F][0-9a-fA-F][0-9a-fA-F]*) ;;
                        *) return 1 ;;
                    esac
                    printf -v hexchar '%b' "\\u${rest:0:4}"
                    value+=$hexchar
                    rest=${rest:4}
                    ;;
                *) value+=$char ;;
            esac
            continue
        fi
        if [ "$char" = '"' ]; then
            printf -v "$out_name" '%s' "$value"
            [ -z "$tail_name" ] || printf -v "$tail_name" '%s' "$rest"
            return 0
        fi
        value+=$char
    done
    return 1
}

# mcp_flag_string NAME — a string flag's value, unquoted and unescaped.
# The raw slice is decoded exactly once (the value ends at the first
# quote no backslash escapes, so a quote inside it decodes rather than
# truncating the read); an unterminated value fails inside the one
# decoder.
mcp_flag_string() {
    local value decoded
    value=$(_mcp_flag_slice "$1")
    case $value in
        \"*) ;;
        *) mcp_fail "flag '$1' is not a string: ${value%%,*}" ;;
    esac
    _mcp_decode_string "$value" decoded ||
        mcp_fail "flag '$1' is not a terminated JSON string: $value"
    printf '%s' "$decoded"
}

# mcp_flag_int NAME — an integer flag's value.
mcp_flag_int() {
    local value
    value=$(_mcp_flag_value "$1")
    case $value in
        '' | *[!0-9]*) mcp_fail "flag '$1' is not an integer: $value" ;;
    esac
    printf '%s' "$value"
}

# mcp_flag_bool NAME — a boolean flag's value: `true` or `false`.
mcp_flag_bool() {
    local value
    value=$(_mcp_flag_value "$1")
    case $value in
        true | false) printf '%s' "$value" ;;
        *) mcp_fail "flag '$1' is not a boolean: $value" ;;
    esac
}

# mcp_flag_list NAME — a repeatable flag's values, one per line (nothing
# when the list is empty). Each entry is a string the one decoder reads
# (an entry may contain a comma or a quote), and the walker skips the
# decoder's reported tail to the next entry.
mcp_flag_list() {
    local value rest entry tail
    value=$(_mcp_flag_slice "$1")
    case $value in
        \[*\]*) ;;
        \[*) mcp_fail "flag '$1' is not a terminated list: $value" ;;
        *) mcp_fail "flag '$1' is not a list: ${value%%,*}" ;;
    esac
    # The array runs to its closing bracket — the first `]`, the
    # pre-existing boundary (an entry holding a bare `]` inside quotes
    # truncates here; the manifest's flag tables carry identifiers, not
    # brackets). The gate above proved the bracket is there, so the cut
    # never turns an unterminated array into a shorter list.
    value=${value#\[}
    value=${value%%\]*}
    [ -z "$value" ] && return 0
    rest=$value
    while :; do
        _mcp_decode_string "$rest" entry tail ||
            mcp_fail "flag '$1' holds an unterminated entry: $rest"
        printf '%s\n' "$entry"
        case $tail in
            '') return 0 ;;
            ,*) rest=${tail#,} ;;
            *) mcp_fail "flag '$1' holds a non-string entry: $tail" ;;
        esac
    done
}

# mcp_init [NAME] [VERSION] — read the host's envelope, complete the
# initialize handshake, and tell the host the client is ready.
mcp_init() {
    local name=${1:-extension} version=${2:-0.1.0}
    # Duplicate the host's pipe before anything can be captured: stdout is
    # the MCP request channel from here on.
    exec 3>&1
    MCP_FD=3
    IFS= read -r MCP_ENVELOPE ||
        mcp_fail "the host wrote no request envelope"
    MCP_ID=1
    printf '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"%s","capabilities":{},"clientInfo":{"name":"%s","version":"%s"}}}\n' \
        "$MCP_PROTOCOL_VERSION" "$name" "$version" >&3
    IFS= read -r MCP_INIT_RESULT ||
        mcp_fail "the host closed the session before the initialize response"
    case $MCP_INIT_RESULT in
        *'"error":'*) mcp_fail "initialize refused: $MCP_INIT_RESULT" ;;
        *'"result":'*) ;;
        *) mcp_fail "initialize answered no result: $MCP_INIT_RESULT" ;;
    esac
    # MCP's rule for a revision the server does not serve: it answers with
    # the one it does, and a client that cannot speak that one must fail the
    # handshake — a session the two sides disagree about is worse than none.
    case $MCP_INIT_RESULT in
        *'"protocolVersion":"'"$MCP_PROTOCOL_VERSION"'"'*) ;;
        *)
            mcp_fail "initialize answered a protocolVersion other than '$MCP_PROTOCOL_VERSION': $MCP_INIT_RESULT"
            ;;
    esac
    # The standard follow-up; the host reads notifications and ignores them.
    printf '{"jsonrpc":"2.0","method":"notifications/initialized"}\n' >&3
}

# mcp_call TOOL ARGS_JSON — one `tools/call`, its arguments the JSON
# object ARGS_JSON. Prints the tool's own JSON result (the MCP text
# content, unescaped); a JSON-RPC error or an unreadable response exits
# the child through `mcp_fail`.
mcp_call() {
    local tool=$1 arguments=${2:-'{}'} response
    case $arguments in
        \{*) ;;
        *) mcp_fail "mcp_call '$tool': the arguments must be a JSON object" ;;
    esac
    [ -n "$MCP_FD" ] || mcp_fail "mcp_call '$tool' before mcp_init"
    MCP_ID=$((MCP_ID + 1))
    printf '{"jsonrpc":"2.0","id":%d,"method":"tools/call","params":{"name":"%s","arguments":%s}}\n' \
        "$MCP_ID" "$tool" "$arguments" >&3
    IFS= read -r response ||
        mcp_fail "the host closed the session before answering '$tool'"
    case $response in
        *'"error":'*) mcp_fail "'$tool' refused: $response" ;;
        *'"content":'*) ;;
        *) mcp_fail "'$tool' answered no tool content: $response" ;;
    esac
    case $response in
        *'"text":"'*) ;;
        *) mcp_fail "'$tool' answered no text content: $response" ;;
    esac
    # The tool's result is the `text` member, read by the kit's one
    # string reader (`mcp_string`): the member is found by name — key
    # order never matters, so the kit does not couple to the host's
    # member order (ADR-0634) — and the value ends at the first quote no
    # backslash escapes, with an unterminated value failing inside the
    # decoder, the one home of that predicate.
    mcp_string "$response" text
}

# mcp_number JSON NAME — the number the JSON object JSON carries under
# NAME (the counters, the read counts). Empty when absent.
mcp_number() {
    local rest=$1
    case $1 in
        *"\"$2\":"*) rest=${1#*\"$2\":} ;;
        *) return 0 ;;
    esac
    rest=${rest%%,*}
    printf '%s' "${rest%%\}*}"
}

# mcp_string JSON NAME — the string the JSON object JSON carries under NAME,
# with the escapes `json_string` writes undone. Empty when absent. The
# value ends at the first quote no backslash escapes, so a quote inside
# it decodes rather than truncating the read; a present-but-unterminated
# value fails through the one decoder, with the member named here.
mcp_string() {
    local rest decoded
    case $1 in
        *"\"$2\":\""*) rest=${1#*\"$2\":\"} ;;
        *) return 0 ;;
    esac
    # Only the opening quote is reconstituted: the member strip consumed
    # it, and the decoder reads to the value's own closing quote — a
    # synthetic one would mask an unterminated response as terminated.
    _mcp_decode_string "\"$rest" decoded ||
        mcp_fail "the '$2' member is not a terminated JSON string"
    printf '%s' "$decoded"
}

# json_string VALUE — VALUE as a JSON string literal, quotes included: the
# argument objects this kit sends are JSON text, so a value from the
# envelope (a flag) is encoded, never spliced in raw. The one writer the
# one decoder mirrors: the same named escapes, and every other control
# character as \u00XX, so a decoded value round-trips byte-exactly and a
# control character never reaches the wire raw.
json_string() {
    local value=$1 out= char code
    while [ -n "$value" ]; do
        char=${value:0:1}
        value=${value:1}
        case $char in
            $'\\') out+='\\' ;;
            '"') out+='\"' ;;
            $'\n') out+='\n' ;;
            $'\r') out+='\r' ;;
            $'\t') out+='\t' ;;
            $'\b') out+='\b' ;;
            $'\f') out+='\f' ;;
            [[:cntrl:]])
                # A control character the shortcuts have no name for:
                # its \u00XX escape, from the code point printf reads
                # off the quoted char. The named escapes above take
                # their characters first.
                printf -v code '%04x' "'$char"
                out+="\\u$code"
                ;;
            *) out+=$char ;;
        esac
    done
    printf '"%s"' "$out"
}
# <<< mcp-client.sh <<<

main "$@"
