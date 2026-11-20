#!/usr/bin/env bash
# mcp-client.sh — the bash half of the MCP mount's client kit (ADR-0447).
#
# Source it from a bash extension and call the host's tools:
#
#     source mcp-client.sh
#     mcp_init my-extension 0.1.0
#     created=$(mcp_call update_schema '{"statement":"CREATE INDEX …"}')
#     mcp_call write_cypher '{"statement":"…","parameters":{"rows":[…]}}' >/dev/null
#     mcp_call report_output '{"text":"imported …"}' >/dev/null
#
# A package ships the manifest plus ONE executable, so an installed package
# cannot source a second file: `so-to-graph.sh` inlines this kit verbatim
# between its markers, and scripts/check-extension-examples.sh pins the two
# copies together.
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
