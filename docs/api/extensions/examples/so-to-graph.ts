#!/usr/bin/env node
/**
 * so-to-graph — the hello-world MCP extension in TypeScript: Stack Overflow
 * questions into a graph.
 *
 * An extension package is the MCP *client*: the host starts this process,
 * writes the request envelope (the resolved `--tag`/`--pages`/`--body`
 * values, one JSON line) on its stdin, and from then on the two pipes carry
 * JSON-RPC 2.0 — this process drives the host's tools (`update_schema`,
 * `write_cypher`, `read_cypher`, `batch_cypher`, `report_output`) and the
 * host alone owns the store.
 *
 * The corpus is live: the public Stack Exchange API's newest questions for
 * one tag, fetched with the corpus's standard filter — answers, owners,
 * tags and `body_markdown` embedded.
 * `--pages N` walks API pages 1..N, each a fresh request (the API's page size
 * is fixed at 100). The schema and the load are that notebook's sections 2-3:
 * a `SOGraph` graph type declared and promoted through `update_schema`, then
 * the API's rows as `(:Question)`, `(:Answer)`, `(:User)` nodes with `ASKED`,
 * `TAGGED`, `ANSWERED` and `PROVIDED` edges.
 *
 * `--body` writes the API's own `body_markdown` onto the question and answer
 * nodes (declared in the graph type, so the schema and the data agree).
 *
 * Data: Stack Overflow through the
 * [Stack Exchange API](https://api.stackexchange.com/docs/questions); Stack
 * Overflow content is licensed
 * [CC BY-SA 4.0](https://creativecommons.org/licenses/by-sa/4.0/), and this
 * copy carries the same license. A question's `id` links back to
 * `https://stackoverflow.com/q/<id>`. Needs network access, and a fresh
 * store (the promoted graph type refuses a re-import of the same questions).
 *
 * Targeted SDK: **@modelcontextprotocol/client 2.x** (verified against
 * 2.4.0; the v2 SDK split into `@modelcontextprotocol/client` and
 * `@modelcontextprotocol/server`). The session, the handshake, the JSON-RPC
 * framing and the error mapping are the SDK's `Client` and its
 * `ReadBuffer`/`serializeMessage` helpers; the only piece the SDK does not
 * ship is a client transport over *this* process's own stdio (its
 * `StdioClientTransport` spawns a server), so `ProcessStdioTransport`
 * implements the SDK's `Transport` interface over the pipes we were handed.
 *
 * Docs: https://github.com/modelcontextprotocol/typescript-sdk/blob/main/docs/advanced/custom-transports.md
 * and https://github.com/modelcontextprotocol/typescript-sdk/blob/main/docs/clients/connect.md
 *
 * Install it as a package (see `quickstart.md`): the package directory holds
 * this file, its `extension.json` and a `package.json` with `"type":
 * "module"` and the SDK dependency, `npm install` runs there, and the
 * directory is placed under the extension root by hand — `extension install
 * --from` copies exactly the manifest and the executable, and the SDK's
 * `node_modules` is neither.
 *
 *     relate run so-to-graph-ts --tag neo4j --pages 1 --body -d ./so
 *
 * Requires Node 22.18+ (TypeScript runs natively) and the host's
 * `initialize` result carrying the standard `protocolVersion` and
 * `capabilities` fields the SDK validates (`spec.md`, "Handshake").
 */

import { createInterface } from "node:readline";
import { Client, ReadBuffer, serializeMessage } from "@modelcontextprotocol/client";
import type {
  JSONRPCMessage,
  Transport,
  TransportSendOptions,
} from "@modelcontextprotocol/client";

const NAME = "so-to-graph-ts";
const VERSION = "0.1.0";

/** The host's request envelope: the manifest's flags, resolved. */
interface Envelope {
  tag: string;
  pages: number;
  body: boolean;
}

/** The public Stack Exchange API, the filter the notebook's generator uses. */
const API = (tag: string, page: number): string =>
  "https://api.stackexchange.com/2.2/questions" +
  `?pagesize=100&order=desc&sort=creation&page=${page}&tagged=${encodeURIComponent(tag)}` +
  "&site=stackoverflow&filter=!5-i6Zw8Y)4W7vpy91PMYsKM-k9yzEsSC1_Uxlf";

/** One API page's payload (the fields the load reads, plus the quota line). */
interface ApiPage {
  items: ApiQuestion[];
  has_more?: boolean;
}

/** One post's owner, as the filter returns it. */
interface ApiOwner {
  user_id: number;
  display_name: string;
  reputation?: number;
  user_type?: string;
  link?: string;
}

/** One answer row, as the filter returns it. */
interface ApiAnswer {
  answer_id: number;
  question_id: number;
  score: number;
  is_accepted: boolean;
  link?: string;
  body_markdown?: string;
  owner: ApiOwner;
}

/** One question row, as the filter returns it (answers embedded). */
interface ApiQuestion {
  question_id: number;
  title: string;
  score: number;
  view_count?: number;
  answer_count: number;
  is_answered?: boolean;
  link?: string;
  body_markdown?: string;
  tags: string[];
  owner: ApiOwner;
  answers?: ApiAnswer[];
}

/** The generated notebook's graph type, promoted to derive its constraints. */
const GRAPH_TYPE = `CREATE GRAPH TYPE SOGraph AS {
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
}`;

const PROMOTE = "ALTER CURRENT GRAPH TYPE SET GRAPH TYPE SOGraph";

/**
 * The questions load, one statement for the whole row set — the reference
 * pattern (its `apoc.load.json` becomes the tool's `$rows` parameter, which
 * is what replaced the retired wire's batch/rows records).
 */
const questionsStatement = (body: boolean): string => `UNWIND $rows AS r
MERGE (q:Question {uuid: r.question_id})
ON CREATE SET q.score = r.score, q.view_count = r.view_count,
              q.answer_count = r.answer_count, q.is_answered = r.is_answered,
              q.link = r.link${body ? ", q.body_markdown = r.body_markdown" : ""}
FOREACH (name IN r.tags |
  MERGE (t:Tag {name: name})
  CREATE (q)-[:TAGGED]->(t))
MERGE (asker:User {uuid: r.owner.user_id})
ON CREATE SET asker.display_name = r.owner.display_name,
              asker.reputation = r.owner.reputation,
              asker.user_type = r.owner.user_type, asker.link = r.owner.link
MERGE (asker)-[:ASKED]->(q)`;

/**
 * The answers, the reference statement's shape: MERGE the Answer, MERGE its
 * Question, MERGE the ANSWERED edge, then MERGE the answerer (ON CREATE SET
 * its profile) and the PROVIDED edge, returning the count.
 */
const answersStatement = (body: boolean): string => `UNWIND $rows AS value
MERGE (a:Answer {uuid: value.answer_id})
ON CREATE SET a.is_accepted = value.is_accepted, a.link = value.link,
              a.score = value.score${body ? ", a.body_markdown = value.body_markdown" : ""}
MERGE (q:Question {uuid: value.question_id})
MERGE (a)-[:ANSWERED]->(q)
WITH a AS answer, value.owner AS owner
MERGE (u:User {uuid: owner.user_id})
ON CREATE SET u.display_name = owner.display_name,
              u.reputation = owner.reputation,
              u.user_type = owner.user_type, u.link = owner.link
MERGE (u)-[:PROVIDED]->(answer)
RETURN count(answer) AS answers`;

/** The notebook's own summary read: questions, answers, tags and users. */
const VERIFY = `MATCH (q:Question)
WITH count(q) AS questions
RETURN questions, COUNT { (:Answer) } AS answers, COUNT { (:Tag) } AS tags,
       COUNT { (:User) } AS users`;

/** The notebook's starter queries: tag co-occurrence, and who answers. */
const STARTERS = [
  `MATCH (t1:Tag)<-[:TAGGED]-(q:Question)-[:TAGGED]->(t2:Tag)
WHERE t1.name < t2.name
RETURN t1.name AS tag, t2.name AS co_occurs, count(q) AS questions
ORDER BY questions DESC, tag, co_occurs LIMIT 10`,
  `MATCH (u:User)-[:PROVIDED]->(:Answer)-[:ANSWERED]->(:Question)-[:TAGGED]->(t:Tag)
RETURN u.display_name AS user, count(*) AS answers
ORDER BY answers DESC, user LIMIT 10`,
];

/** Pages 1..`pages` of the API's newest questions for `tag`, accumulated. */
async function fetchQuestions(tag: string, pages: number): Promise<ApiQuestion[]> {
  const questions: ApiQuestion[] = [];
  for (let page = 1; page <= pages; page += 1) {
    // `fetch` (undici) decodes the API's gzip response body for us.
    const response = await fetch(API(tag, page));
    if (!response.ok) {
      throw new Error(`the API answered ${response.status} for ${tag} page ${page}`);
    }
    const payload = (await response.json()) as ApiPage;
    questions.push(...payload.items);
    if (!payload.has_more) break;
  }
  return questions;
}

async function run(): Promise<void> {
  // The envelope is read before the transport takes stdin over: the host writes
  // it first, and the session's line reader must not see it.
  const envelope = JSON.parse(await readEnvelope()) as Envelope;
  const transport = new ProcessStdioTransport();
  const client = new Client({ name: NAME, version: VERSION });
  await client.connect(transport);
  // The host resolves the manifest's defaults (tag `neo4j`, pages `1`) into
  // the envelope, so an empty tag or a non-positive page count is an explicit
  // value the run refuses rather than overwriting silently.
  if (envelope.tag === "") {
    throw new Error("flag 'tag' is empty");
  }
  if (envelope.pages < 1) {
    throw new Error(`flag 'pages' must be at least 1: ${envelope.pages}`);
  }
  const tag = envelope.tag;
  const pages = envelope.pages;
  const body = envelope.body;
  const questions = await fetchQuestions(tag, pages);
  const answers = questions.flatMap((question) => question.answers ?? []);
  if (questions.length === 0) {
    throw new Error(`the API returned no questions for ${tag}`);
  }

  // 1. The notebook's schema: the graph type, then its promotion, which
  // derives the enforcing constraints.
  for (const statement of [GRAPH_TYPE, PROMOTE]) {
    toolJson(await client.callTool({ name: "update_schema", arguments: { statement } }));
  }

  // 2. The load, one call: the two statements of the notebook's pass —
  // questions (and their tags and askers), then the answers (and their
  // answerers) — in the one transaction `batch_cypher` gives the call.
  toolJson(
    await client.callTool({
      name: "batch_cypher",
      arguments: {
        statements: [
          { statement: questionsStatement(body), parameters: { rows: questions } },
          { statement: answersStatement(body), parameters: { rows: answers } },
        ],
      },
    }),
  );

  // 3. The promotion's derived key constraint is what the schema step
  // asserts (a graph type is no index or constraint row, so `created` stays
  // empty): a duplicate question key must now be refused.
  let duplicateRefused = false;
  try {
    await client.callTool({
      name: "write_cypher",
      arguments: {
        statement: "CREATE (q:Question {uuid: $uuid, score: 0})",
        parameters: { uuid: questions[0].question_id },
      },
    });
  } catch {
    // A per-call refusal arrives as a JSON-RPC error the SDK throws.
    duplicateRefused = true;
  }
  if (!duplicateRefused) {
    throw new Error("the promoted graph type admitted a duplicate key");
  }

  // 4. The notebook's own summary read: what actually landed.
  const read = toolJson(
    await client.callTool({ name: "read_cypher", arguments: { statement: VERIFY } }),
  );
  const firstRow = Array.isArray(read.rows) ? read.rows[0] : undefined;
  const row = isRecord(firstRow) ? firstRow : {};
  const wanted: Record<string, number> = {
    questions: questions.length,
    answers: answers.length,
    tags: new Set(questions.flatMap((question) => question.tags)).size,
    users: new Set([
      ...questions.map((question) => question.owner.user_id),
      ...answers.map((answer) => answer.owner.user_id),
    ]).size,
  };
  for (const [column, expected] of Object.entries(wanted)) {
    const actual = row[column];
    if (typeof actual !== "number" || actual !== expected) {
      throw new Error(`the graph holds ${String(actual)} ${column}, expected ${expected}`);
    }
  }

  // 5. The summary: the counts, then the notebook's two starter queries.
  const note = body ? " with bodies" : "";
  await client.callTool({
    name: "report_output",
    arguments: {
      text:
        `imported ${wanted.questions} questions, ${wanted.answers} answers, ` +
        `${wanted.tags} tags and ${wanted.users} users tagged '${tag}'${note}`,
      starters: STARTERS,
    },
  });

  await client.close();
}

// ---------------------------------------------------------------------------
// Boilerplate: the mount's plumbing. Nothing below says what this extension
// DOES — it is the MCP transport over the host's pipes, the envelope read,
// and the result scratch. Copy it as-is (or use the SDK's own transport
// seam) in your own extension.
// ---------------------------------------------------------------------------

/**
 * The SDK's `Transport` over this process's own stdio: the SDK's own
 * `ReadBuffer` reads the newline-delimited lines and `serializeMessage`
 * writes them. `StdioClientTransport` runs exactly this framing against a
 * process it spawned; the host here spawned *us*.
 */
class ProcessStdioTransport implements Transport {
  onclose?: () => void;
  onerror?: (error: Error) => void;
  onmessage?: <T extends JSONRPCMessage>(message: T) => void;

  private buffer = new ReadBuffer();

  async start(): Promise<void> {
    process.stdin.on("data", (chunk: Buffer) => {
      this.buffer.append(chunk);
      for (
        let message = this.buffer.readMessage();
        message !== null;
        message = this.buffer.readMessage()
      ) {
        this.onmessage?.(message);
      }
    });
    process.stdin.on("end", () => this.onclose?.());
    process.stdin.resume();
  }

  async send(message: JSONRPCMessage, _options?: TransportSendOptions): Promise<void> {
    // `serializeMessage` carries the line terminator itself.
    process.stdout.write(serializeMessage(message));
  }

  async close(): Promise<void> {
    process.stdin.pause();
    this.onclose?.();
  }
}

/**
 * The host's request envelope: one line, read before the session starts and
 * before the transport takes stdin over. The host writes nothing else until
 * it has the initialize request, so a line reader cannot race the session
 * for bytes.
 */
async function readEnvelope(): Promise<string> {
  const lines = createInterface({ input: process.stdin, crlfDelay: Infinity });
  const envelope = await new Promise<string>((resolve) => {
    lines.once("line", resolve);
  });
  lines.close();
  process.stdin.pause();
  return envelope;
}

/** What `CallTool` answers, the SDK's own type for it. */
type ToolResult = Awaited<ReturnType<Client["callTool"]>>;

/** The tool's JSON payload, which the host carries as `content[0].text`. */
function toolJson(result: ToolResult): Record<string, unknown> {
  if (result.isError === true) {
    throw new Error(`the host refused the call: ${JSON.stringify(result)}`);
  }
  // The SDK's result type has a compatibility arm without `content`; the
  // host always answers the standard one.
  const blocks = "content" in result && Array.isArray(result.content) ? result.content : [];
  for (const block of blocks) {
    if (block.type === "text") {
      const parsed: unknown = JSON.parse(block.text);
      if (!isRecord(parsed)) {
        throw new Error(`the tool text is not a JSON object: ${block.text}`);
      }
      return parsed;
    }
  }
  throw new Error(`the tool answered no text content: ${JSON.stringify(result)}`);
}

/** Whether `value` is a plain JSON object — the one narrowing for host data. */
function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

run().catch((error: unknown) => {
  process.stderr.write(`so-to-graph: ${error instanceof Error ? error.message : String(error)}\n`);
  // Exit, not `exitCode`: the stdin handle would keep the process alive and the
  // host waiting for requests that never come.
  process.exit(1);
});
