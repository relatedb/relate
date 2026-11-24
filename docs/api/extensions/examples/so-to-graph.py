#!/usr/bin/env python3
"""so-to-graph — the hello-world MCP extension in Python: Stack Overflow
questions into a graph.

An extension package is the MCP *client*: the host starts this process,
writes the request envelope (the resolved ``--tag``/``--pages``/``--body``
values, one JSON line) on its stdin, and from then on the two pipes carry
JSON-RPC 2.0 — this process drives the host's tools (``update_schema``,
``write_cypher``, ``read_cypher``, ``batch_cypher``, ``report_output``) and
the host alone owns the store.

The corpus is live: the public Stack Exchange API's newest questions for one
tag, fetched with the corpus's standard filter — answers, owners, tags
and ``body_markdown`` embedded.
``--pages N`` walks API pages 1..N, each a fresh request (the API's page size
is fixed at 100). The schema and the load are the notebook's (its sections
2-3): a ``SOGraph`` graph type declared and promoted through
``update_schema`` — questions, answers, users and tags with their typed
properties and the four relationship types — and then the API's rows as
``(:Question)``, ``(:Answer)``, ``(:User)`` nodes with ``ASKED``,
``TAGGED``, ``ANSWERED`` and ``PROVIDED`` edges.

Data: Stack Overflow through the
[Stack Exchange API](https://api.stackexchange.com/docs/questions); Stack
Overflow content is licensed
[CC BY-SA 4.0](https://creativecommons.org/licenses/by-sa/4.0/), and this
copy carries the same license. A question's ``id`` links back to
``https://stackoverflow.com/q/<id>``. Needs network access, and a fresh
store (the constraints refuse a re-import of the same questions).

Targeted SDK: **mcp 2.x** (verified against 2.3.0). The client is created
with ``mode="legacy"``: this host serves the handshake-era protocol, so the
SDK must open with the ``initialize`` handshake instead of its modern
``server/discover`` probe. The session, the handshake, the protocol
negotiation, the JSON-RPC framing and the error mapping are the SDK's
``mcp.Client``; the only piece the SDK does not ship is a client transport
over *this* process's own stdin/stdout (its ``stdio_client`` spawns a server
instead), so ``OwnStdio`` bridges those two pipes through the SDK's
``Transport`` protocol and its own line parser
(``mcp_types.jsonrpc_message_adapter``). A shipped child-side stdio
transport would remove even that adapter.

``--body`` writes the API's own ``body_markdown`` onto the question and
answer nodes (declared in the graph type, so the schema and the data agree).

Docs: https://modelcontextprotocol.io/docs/2026-07-28/develop/build-client
and https://modelcontextprotocol.io/docs/2026-07-28/clients/client-best-practices

Install it as a package (see ``quickstart.md``)::

    relate extension install so-to-graph-py --from ./so-to-graph-py
    relate run so-to-graph-py --tag neo4j --pages 1 --body -d ./so

Requires the ``mcp`` SDK (``pip install 'mcp>=2'``); the host's
``initialize`` result carries the standard ``protocolVersion`` and
``capabilities`` fields the SDK validates (``spec.md``, "Handshake").
"""

from __future__ import annotations

import gzip
import json
import os
import sys
import urllib.request
from collections.abc import AsyncGenerator
from contextlib import AbstractAsyncContextManager, asynccontextmanager
from types import TracebackType
from typing import Any

import anyio
import mcp_types
from mcp import Client
from mcp.client._transport import TransportStreams
from mcp.shared.message import SessionMessage

NAME = "so-to-graph-py"
VERSION = "0.1.0"

# The public Stack Exchange API, the filter the cookbook notebook's generator
# uses: answers, owners, tags and body_markdown embedded. The API's page size
# is fixed at 100; ``page`` walks the newest first.
API = (
    "https://api.stackexchange.com/2.2/questions"
    "?pagesize=100&order=desc&sort=creation&page={page}&tagged={tag}"
    "&site=stackoverflow&filter=!5-i6Zw8Y)4W7vpy91PMYsKM-k9yzEsSC1_Uxlf"
)

# The corpus's graph type: the typed schema of questions, answers, users
# and tags, with the four relationship types. Promoting it derives the
# enforcing constraints.
GRAPH_TYPE = """CREATE GRAPH TYPE SOGraph AS {
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
}"""

PROMOTE = "ALTER CURRENT GRAPH TYPE SET GRAPH TYPE SOGraph"

# The load, one statement per row kind — the reference pattern (its
# `apoc.load.json` becomes the tool's `$rows` parameter, which is what
# replaced the retired wire's batch/rows records). The questions: one
# Question per API row (uuid = question_id), one Tag merged per tag name, and
# the asker merged as a User with ASKED.
QUESTIONS = """UNWIND $rows AS r
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
MERGE (asker)-[:ASKED]->(q)"""

# The answers, the reference statement's shape: MERGE the Answer, MERGE its
# Question, MERGE the ANSWERED edge, then MERGE the answerer (ON CREATE SET
# its profile) and the PROVIDED edge, returning the count.
ANSWERS = """UNWIND $rows AS value
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
RETURN count(answer) AS answers"""

# The notebook's own summary read: questions, answers, tags and users.
VERIFY = """MATCH (q:Question)
WITH count(q) AS questions
RETURN questions, COUNT { (:Answer) } AS answers, COUNT { (:Tag) } AS tags,
       COUNT { (:User) } AS users"""

# The notebook's starter queries: tag co-occurrence, and who answers the
# tag's questions.
STARTERS = [
    """MATCH (t1:Tag)<-[:TAGGED]-(q:Question)-[:TAGGED]->(t2:Tag)
WHERE t1.name < t2.name
RETURN t1.name AS tag, t2.name AS co_occurs, count(q) AS questions
ORDER BY questions DESC, tag, co_occurs LIMIT 10""",
    """MATCH (u:User)-[:PROVIDED]->(:Answer)-[:ANSWERED]->(:Question)-[:TAGGED]->(t:Tag)
RETURN u.display_name AS user, count(*) AS answers
ORDER BY answers DESC, user LIMIT 10""",
]


def fetch_page(tag: str, page: int) -> dict[str, Any]:
    """One API page: its questions (answers embedded) and the API's quota line."""
    request = urllib.request.Request(API.format(page=page, tag=tag))
    # The API gzips when asked to; ask, and decode what comes back.
    request.add_header("Accept-Encoding", "gzip")
    with urllib.request.urlopen(request, timeout=30) as response:
        body = response.read()
        if response.headers.get("Content-Encoding") == "gzip":
            body = gzip.decompress(body)
    return json.loads(body.decode("utf-8"))


def fetch_questions(tag: str, pages: int) -> list[dict[str, Any]]:
    """Pages 1..`pages` of the API's newest questions for `tag`, accumulated."""
    questions: list[dict[str, Any]] = []
    for page in range(1, pages + 1):
        payload = fetch_page(tag, page)
        questions.extend(payload["items"])
        if not payload.get("has_more", False):
            break
    return questions


def answer_rows(questions: list[dict[str, Any]]) -> list[dict[str, Any]]:
    """Every answer of the fetched questions, as one flat list of rows."""
    return [answer for question in questions for answer in question.get("answers", [])]


async def run() -> None:
    transport = OwnStdio()
    # `mode="legacy"`: the host serves the handshake-era protocol (the
    # `initialize` handshake), so the client must not open with a modern
    # `server/discover` probe first.
    async with Client(transport, mode="legacy", raise_exceptions=True) as client:
        envelope = transport.envelope
        if envelope is None:  # the transport reads it before the session
            raise SystemExit("so-to-graph: the transport read no request envelope")
        # The host resolves the manifest's defaults (tag ``neo4j``, pages
        # ``1``) into the envelope, so an empty tag or a non-positive page
        # count is an explicit value the run refuses rather than overwriting
        # silently.
        tag = envelope["tag"]
        pages = int(envelope["pages"])
        if not tag:
            raise SystemExit("so-to-graph: flag 'tag' is empty")
        if pages < 1:
            raise SystemExit(f"so-to-graph: flag 'pages' must be at least 1: {pages}")
        # `--body` writes the API's own markdown body onto the question and
        # onto the ANSWERED relationship (the model has no Answer node).
        body = bool(envelope["body"])
        questions = fetch_questions(tag, pages)
        answers = answer_rows(questions)
        if not questions:
            raise SystemExit(f"so-to-graph: the API returned no questions for {tag!r}")

        # 1. The notebook's schema: the graph type, then its promotion, which
        # derives the enforcing constraints.
        for statement in (GRAPH_TYPE, PROMOTE):
            tool_json(await client.call_tool("update_schema", {"statement": statement}))

        # 2. The load, one call: the two statements of the notebook's pass —
        # questions (and their tags and askers), then the answers (and their
        # answerers) — in the one transaction `batch_cypher` gives the call.
        body_question = ", q.body_markdown = r.body_markdown" if body else ""
        body_answer = ", a.body_markdown = value.body_markdown" if body else ""
        questions_statement = QUESTIONS.replace("BODY_QUESTION", body_question)
        answers_statement = ANSWERS.replace("BODY_ANSWER", body_answer)
        tool_json(
            await client.call_tool(
                "batch_cypher",
                {
                    "statements": [
                        {
                            "statement": questions_statement,
                            "parameters": {"rows": questions},
                        },
                        {
                            "statement": answers_statement,
                            "parameters": {"rows": answers},
                        },
                    ]
                },
            )
        )

        # 3. The promotion's derived key constraint is what the schema step
        # asserts (a graph type is no index or constraint row, so `created`
        # stays empty): a duplicate question key must now be refused.
        duplicate_refused = False
        try:
            duplicate = await client.call_tool(
                "write_cypher",
                {
                    "statement": "CREATE (q:Question {uuid: $uuid, score: 0})",
                    "parameters": {"uuid": questions[0]["question_id"]},
                },
            )
            duplicate_refused = bool(getattr(duplicate, "is_error", False))
        except Exception:  # a per-call refusal arrives as a JSON-RPC error
            duplicate_refused = True
        if not duplicate_refused:
            raise SystemExit("so-to-graph: the promoted graph type admitted a duplicate key")

        # 4. The notebook's own summary read: what actually landed.
        found = tool_json(await client.call_tool("read_cypher", {"statement": VERIFY}))
        row = found["rows"][0] if found["rows"] else {}
        wanted = {
            "questions": len(questions),
            "answers": len(answers),
            "tags": len({name for question in questions for name in question["tags"]}),
            "users": len(
                {question["owner"]["user_id"] for question in questions}
                | {answer["owner"]["user_id"] for answer in answers}
            ),
        }
        for column, expected_count in wanted.items():
            if row.get(column) != expected_count:
                raise SystemExit(
                    f"so-to-graph: the graph holds {row.get(column)} {column}, "
                    f"expected {expected_count}"
                )

        # 5. The summary: the counts, then the notebook's two starter queries.
        note = " with bodies" if body else ""
        await client.call_tool(
            "report_output",
            {
                "text": (
                    f"imported {wanted['questions']} questions, {wanted['answers']} answers, "
                    f"{wanted['tags']} tags and {wanted['users']} users tagged {tag!r}{note}"
                ),
                "starters": STARTERS,
            },
        )


def leaf_errors(error: BaseException) -> list[str]:
    """The messages under however many task-group wrappers, outermost first."""
    if isinstance(error, BaseExceptionGroup):
        return [leaf for inner in error.exceptions for leaf in leaf_errors(inner)]
    return [str(error).splitlines()[0] if str(error) else type(error).__name__]


def main() -> None:
    try:
        anyio.run(run)
    except SystemExit:
        raise
    except BaseExceptionGroup as group:
        # A failed handshake or a broken pipe reaches the host as one line.
        raise SystemExit(f"so-to-graph: {'; '.join(leaf_errors(group))}") from group
    except Exception as error:  # the host reads this line, not a traceback
        raise SystemExit(f"so-to-graph: {error}") from error


# ---------------------------------------------------------------------------
# Boilerplate: the mount's plumbing. Nothing below says what this extension
# DOES — it is the MCP transport over the host's pipes, the envelope read,
# and the two result scratches. Copy it as-is (or lift the equivalent from
# the SDKs' own transports) into your own extension.
# ---------------------------------------------------------------------------


def read_envelope(stream: Any = None) -> dict[str, Any]:
    """The host's request envelope: one JSON line on stdin, before the session.

    Reading it is the transport's first act (the host writes it before it
    reads a single request); the returned mapping is the flag values the
    manifest declares.
    """
    stream = sys.stdin if stream is None else stream
    line = stream.readline()
    if not line:
        raise SystemExit("so-to-graph: the host wrote no request envelope")
    return json.loads(line)


def parse_message(line: str) -> SessionMessage:
    """One JSON-RPC line as a session message — the SDK's own parser."""
    return SessionMessage(mcp_types.jsonrpc_message_adapter.validate_json(line, by_name=False))


class OwnStdio:
    """The SDK ``Transport`` over this process's own stdin/stdout.

    An async context manager yielding ``(read_stream, write_stream)`` — the
    shape ``mcp.Client`` accepts for any transport. Two tasks bridge the
    pipes: stdin lines in (through the SDK's own parser), session messages
    out (``model_dump_json``, one line each, flushed).
    """

    def __init__(self) -> None:
        self.envelope: dict[str, Any] | None = None
        self._context: AbstractAsyncContextManager[TransportStreams] | None = None

    async def __aenter__(self) -> TransportStreams:
        self._context = self._connect()
        return await self._context.__aenter__()

    async def __aexit__(
        self,
        exc_type: type[BaseException] | None,
        exc_val: BaseException | None,
        exc_tb: TracebackType | None,
    ) -> bool | None:
        assert self._context is not None, "OwnStdio.__aexit__ runs only after __aenter__"
        exited = await self._context.__aexit__(exc_type, exc_val, exc_tb)
        self._context = None
        return exited

    @asynccontextmanager
    async def _connect(self) -> AsyncGenerator[TransportStreams]:
        # The envelope comes first: the host wrote it before the session.
        self.envelope = read_envelope()
        read_writer, read_stream = anyio.create_memory_object_stream[
            SessionMessage | Exception
        ](0)
        write_stream, write_reader = anyio.create_memory_object_stream[SessionMessage](0)

        async def stdin_reader() -> None:
            # A readiness wait, not a blocking read on a worker thread: a
            # thread parked in `read` keeps the interpreter alive after the
            # session ends.
            buffer = b""
            async with read_writer:
                while True:
                    await anyio.wait_readable(sys.stdin.fileno())
                    chunk = os.read(sys.stdin.fileno(), 65536)
                    if not chunk:
                        return
                    *lines, buffer = (buffer + chunk).split(b"\n")
                    for line in lines:
                        if not line.strip():
                            continue
                        try:
                            await read_writer.send(parse_message(line.decode("utf-8")))
                        except ValueError as error:
                            # The SDK surfaces a parse failure as a value the
                            # session reports, not as a crashed reader.
                            await read_writer.send(error)
                        except (anyio.ClosedResourceError, anyio.BrokenResourceError):
                            return

        async def stdout_writer() -> None:
            async with write_reader:
                async for session_message in write_reader:
                    payload = session_message.message.model_dump_json(
                        by_alias=True, exclude_unset=True
                    )
                    sys.stdout.write(payload + "\n")
                    sys.stdout.flush()

        async with anyio.create_task_group() as task_group:
            task_group.start_soon(stdin_reader)
            task_group.start_soon(stdout_writer)
            try:
                yield read_stream, write_stream
            finally:
                task_group.cancel_scope.cancel()


def tool_json(result: Any) -> Any:
    """The tool's JSON payload, which the host carries as ``content[0].text``."""
    if getattr(result, "is_error", False):
        raise SystemExit(f"so-to-graph: the host refused the call: {result}")
    for block in result.content:
        text = getattr(block, "text", None)
        if text is not None:
            return json.loads(text)
    raise SystemExit(f"so-to-graph: the tool answered no text content: {result}")


if __name__ == "__main__":
    main()
