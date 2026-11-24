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
# The last response line `mcp_request` read.
MCP_RESPONSE=

mcp_fail() {
    printf 'mcp: %s\n' "$*" >&2
    exit 1
}

# _mcp_member_rest JSON NAME OUT — the raw text after `"NAME":` in the JSON
# object JSON (the value's first character on — a string value's opening
# quote included), into the variable OUT; returns 1 when JSON carries no
# such member. The one spelling of the member find: the envelope's flag
# readers, the scalar reader and the string reader all consult it, so "the
# member is found by name" has one home — key order never matters, so the
# kit does not couple to the host's member order (ADR-0634).
_mcp_member_rest() {
    case $1 in
        *"\"$2\":"*) printf -v "$3" '%s' "${1#*\"$2\":}" ;;
        *) return 1 ;;
    esac
}

# _mcp_scalar_token TEXT — TEXT's leading scalar token: everything up to
# the next `,`, or to the object's closing `}` when the scalar is the
# last member. The one spelling of "a scalar ends at the next comma or
# the closing brace" — the two cuts stay separate because `}` inside a
# bracket expression ends the expansion — read by `mcp_number` and
# `_mcp_flag_value`, and by the flag readers cutting a mismatch's value
# for display.
_mcp_scalar_token() {
    local rest=${1%%,*}
    printf '%s' "${rest%%\}*}"
}

# _mcp_flag_required NAME OUT — the raw envelope text from flag NAME's
# value on, into the variable OUT, or the one refusal when the manifest and
# the child disagree about the flag table. The slice is always taken inside
# a command substitution, where a `mcp_fail` there would end a subshell
# rather than the child, so the refusal lives here, at the one boundary
# every flag reader shares.
_mcp_flag_required() {
    _mcp_member_rest "$MCP_ENVELOPE" "$1" "$2" ||
        mcp_fail "the request envelope carries no '$1' flag"
}

# The scalar token of flag NAME (`2`, `false`), shaped for the scalar
# readers (`mcp_flag_int`, `mcp_flag_bool`). A string flag is the string
# reader's and a list the list reader's — this reader never decodes,
# re-encodes or brackets anything. Fails when the manifest and the child
# disagree about the flag table.
_mcp_flag_value() {
    local rest
    _mcp_flag_required "$1" rest
    _mcp_scalar_token "$rest"
}

# _mcp_decode_string TOKEN OUT [TAIL] — the value of the JSON string
# literal TOKEN (its opening quote included), escapes undone, into the
# variable OUT; the text after the closing quote, into TAIL when given
# (the list walker needs it). Returns 2 when TOKEN is not a string, and 1
# when its closing quote never comes (an unterminated value is a failure,
# never a silent truncation) or a `\u` escape's four hex digits are
# missing — no message, no exit, so it works wherever it is called,
# command substitutions included; the caller picks its message from the
# status.
_mcp_decode_string() {
    # The out-parameter names are held in dedicated locals (`out_name`,
    # `tail_name`): a local sharing a caller's variable name would make
    # `printf -v` write the local, not the caller's variable.
    local rest=$1 out_name=$2 tail_name=${3-} value= char hexchar
    case $rest in
        \"*) rest=${rest#\"} ;;
        *) return 2 ;;
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

# _mcp_decode_refuse WHO STATUS RAW — the one refusal for a failed
# `_mcp_decode_string` (STATUS is its return status): WHO is the subject
# ("flag 'tag'", "flag 'tag' entry"), RAW the text the decoder read. A
# value that is not a string at all reads differently from an unterminated
# one; both flag readers (the scalar string and the list walker) ask this
# one mapping.
_mcp_decode_refuse() {
    case $2 in
        2) mcp_fail "$1 is not a string: $(_mcp_scalar_token "$3")" ;;
        *) mcp_fail "$1 is not a terminated JSON string: $3" ;;
    esac
}

# mcp_flag_string NAME — a string flag's value, unquoted and unescaped.
# The raw slice is decoded exactly once (the value ends at the first
# quote no backslash escapes, so a quote inside it decodes rather than
# truncating the read); the one decoder's status picks the message — a
# value that is not a string at all reads differently from an
# unterminated one.
mcp_flag_string() {
    local value decoded
    _mcp_flag_required "$1" value
    _mcp_decode_string "$value" decoded
    local status=$?
    [ "$status" -eq 0 ] || _mcp_decode_refuse "flag '$1'" "$status" "$value"
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
    _mcp_flag_required "$1" value
    case $value in
        \[*\]*) ;;
        \[*) mcp_fail "flag '$1' is not a terminated list: $value" ;;
        *) mcp_fail "flag '$1' is not a list: $(_mcp_scalar_token "$value")" ;;
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
            _mcp_decode_refuse "flag '$1' entry" "$?" "$rest"
        printf '%s\n' "$entry"
        case $tail in
            '') return 0 ;;
            ,*) rest=${tail#,} ;;
            *) mcp_fail "flag '$1' holds a non-string entry: $tail" ;;
        esac
    done
}

# mcp_refused RESPONSE — true when the JSON-RPC response RESPONSE carries
# an `error` member: the one refusal predicate the kit and its callers
# share.
mcp_refused() {
    case $1 in
        *'"error":'*) return 0 ;;
        *) return 1 ;;
    esac
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
    mcp_refused "$MCP_INIT_RESULT" && mcp_fail "initialize refused: $MCP_INIT_RESULT"
    case $MCP_INIT_RESULT in
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

# mcp_request TOOL ARGS_JSON — the raw `tools/call` frame: bumps the
# request id, writes the envelope, and leaves the one response line, as
# the host sent it, error or not, in MCP_RESPONSE. Only a host that closes
# the session fails here; `mcp_call` judges the response, and a step that
# expects a refusal reads it itself. It sets a variable rather than
# printing so the id bump survives in the caller's shell.
mcp_request() {
    local tool=$1 arguments=$2
    MCP_ID=$((MCP_ID + 1))
    printf '{"jsonrpc":"2.0","id":%d,"method":"tools/call","params":{"name":"%s","arguments":%s}}\n' \
        "$MCP_ID" "$tool" "$arguments" >&3
    IFS= read -r MCP_RESPONSE ||
        mcp_fail "the host closed the session before answering '$tool'"
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
    mcp_request "$tool" "$arguments"
    response=$MCP_RESPONSE
    mcp_refused "$response" && mcp_fail "'$tool' refused: $response"
    case $response in
        *'"content":'*) ;;
        *) mcp_fail "'$tool' answered no tool content: $response" ;;
    esac
    # The tool's result is the `text` member, read by the kit's one
    # required string reader (`_mcp_string_required`): the member is found
    # by name — key order never matters, so the kit does not couple to
    # the host's member order (ADR-0634) — its value ends at the first
    # quote no backslash escapes (an unterminated value fails inside the
    # decoder, the one home of that predicate), and an absent member is a
    # refusal there, so the response is not scanned for the member first.
    _mcp_string_required "$response" text
}

# mcp_number JSON NAME — the number the JSON object JSON carries under
# NAME (the counters, the read counts). Empty when absent.
mcp_number() {
    local rest
    _mcp_member_rest "$1" "$2" rest || return 0
    _mcp_scalar_token "$rest"
}

# mcp_string JSON NAME — the string the JSON object JSON carries under NAME,
# with the escapes `json_string` writes undone. Prints nothing and returns
# 1 when JSON carries no string member NAME (absent, or not a string at
# all). The value ends at the first quote no backslash escapes, so a quote
# inside it decodes rather than truncating the read; a
# present-but-unterminated value fails through the one decoder, with the
# member named here.
mcp_string() {
    local rest decoded
    _mcp_member_rest "$1" "$2" rest || return 1
    _mcp_decode_string "$rest" decoded
    case $? in
        0) ;;
        2) return 1 ;;
        *) mcp_fail "the '$2' member is not a terminated JSON string" ;;
    esac
    printf '%s' "$decoded"
}

# _mcp_string_required JSON NAME — `mcp_string` with the member required: a
# missing string member is a refusal through `mcp_fail`, so a caller that
# needs the member (the tool result's `text`) requires it in the one read
# instead of scanning the response for it first.
_mcp_string_required() {
    mcp_string "$1" "$2" ||
        mcp_fail "the response carries no '$2' string member: $1"
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
