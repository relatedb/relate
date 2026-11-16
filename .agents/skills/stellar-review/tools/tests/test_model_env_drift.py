"""Drift guard for the review-model env vocabulary (ADR-0302): the four
`STELLAR_MODEL_<PASS>` names are the ones the generator reads
(`pass_models.pass_model_env()`, derived from the canonical production pass
registry `briefs.PASS_LAYOUT`), and every copy of the vocabulary — the
reusable workflow's pass-model step env and workflow_call secrets, its
callers' secrets blocks (every watched `opencode-review-*` workflow but the
reusable one), SKILL.md's MODEL TIERS rule, and the
review-scripts-test path filter — must agree with it. The pass names stay
out of the shared Run OpenCode step, whose only override is
OPENCODE_REVIEW_MODEL. The default model pin the override falls back to
must match ADR-0302's Decision table in both workflows that carry it.

The same file also pins the reusable workflow's three post-model hardening
guarantees (ADR-0603): the Run OpenCode step's env blanks the
`GITHUB_ENV`/`GITHUB_PATH` publish channels (a model shell must not
publish env vars or PATH entries — a BASH_ENV-style payload included —
into the later, token-bearing steps), the three restore call sites
(scripted post, per-kind post, enforce) stage review-restore.sh from the
PR head SHA's committed copy instead of the model-writable worktree,
and every token-bearing post step pins its process-start environment
at the step env level — the complete set of env channels the ELF
loader and the runner's interpreters document for loading code at
process start (PINNED_STARTUP_VARS below), each pinned to the empty
string and PATH pinned to the fixed system list review-restore.sh
pins — because the blanking stops only the model's own file-command
writes (the runner env file under RUNNER_TEMP stays writable by
path), while a step env entry, the most specific Actions env scope,
overrides anything published and holds from process start. The
guarded steps are enumerated from the workflow itself (every step
after the model step whose env maps GH_TOKEN), so adding a step or
renaming a pin in the workflow fails the drift test until every
token-bearing post step carries the full list.

The same step env pins git's configuration file sources (the round-5
finding: the model shell can write .git/config, the worktree-scoped
config and the shared HOME gitconfig, and every git command the post
steps run parses them) — GIT_CONFIG_GLOBAL/GIT_CONFIG_SYSTEM at
/dev/null, GIT_CONFIG_NOSYSTEM on, GIT_CONFIG_COUNT zeroed (the
env-numbered `GIT_CONFIG_KEY_n` pairs are git configuration too,
plantable through the same runner env file). The repository's own
.git/config — the one source those pins leave parsing — is handled
twice over (the round-6 finding: a model-written core.worktree
redirects the reset to a decoy directory, a filter.<driver>.smudge
plus core.attributesFile pair executes during reset --hard, and a
url.*.insteadOf rewrite reaches an ext:: fetch): review-restore.sh
replaces the file outright with the minimal trusted configuration
before its first git invocation (nothing in it sourced from
model-writable state; the origin URL is rebuilt from the REPO env the
steps pass), and every restore-path and fetch git invocation — the
staging `git show`, the helper's reset, clean and ls-files, the prep
fetches, the enforce ls-files — pins its remaining knobs off per
invocation with the six GIT_PIN_SET flags below.
review-restore.sh must export the same config-source pins in-script,
the PATH-pin defense-in-depth precedent.

Round 8 closes the three object-resolution and attribute channels that
survived all of the above: replace objects are disabled (the model
step can create refs/replace/<SHA> entries — the staging git shows and
the helper's reset carry --no-replace-objects, review-restore.sh
exports GIT_NO_REPLACE_OBJECTS=1 before its first git call, and every
token-bearing post step pins the env var from process start);
.git/info/attributes — the one attribute source no pin or replacement
covered — is deleted by review-restore.sh alongside the config
replacement; and every facts-recomputation git diff (facts.py's four
reads and incremental.py's anchoring patch, recomputed by the stellar
and per-kind posts after the model step) carries --no-ext-diff
--no-textconv, so a model-written external diff driver cannot execute
during facts recomputation even when a persistent model-spawned child
rewrites .git/config between git calls.

Round 9 extends the pin set and moves the fetch credential off the
command line; its guards live in test_review_git_hardening.py (the
focused module for the round-9 findings, following the
test_last_head_fetch_guard precedent): the minimal trusted .git/config
the replacement writes carries core.hooksPath = /dev/null — the model
shell can write the default .git/hooks directory, and probed on git
2.43/2.54 an undefended reset --hard executes a model-crafted
.git/hooks/reference-transaction — and the authenticated review
fetches deliver the GitHub credential through git's env-numbered
configuration channel, never a command-line extraheader, reconciled
with the GIT_CONFIG_COUNT=0 pins above (command-scoped assignments of
fixed literals raise the count to exactly 1 for the fetch command
alone). GIT_PIN_SET below gains the hooksPath pin the round-9 guards
count.

The Run OpenCode step keeps --auto (the round-4 permission-abort
class) but runs under explicit deny rules (the round-6 finding): the
ordered MODEL_DENY_POLICIES below, delivered through the
OPENCODE_CONFIG_CONTENT env var — a virtual config layer the runner
authors. They are experimental.policies, not plain permission rules,
because the policy hook forces its deny after rule evaluation: a PR's
own project config (an opencode.json in the reviewed tree, the last
config layer, whose rules would otherwise win) cannot re-allow what
this layer denies.
"""

import glob
import json
import os
import re
import unittest

from helpers import ROOT, read, region_between  # also puts tools/ on sys.path

import pass_models

WORKFLOWS = os.path.join(ROOT, ".github", "workflows")
REUSABLE = os.path.join(WORKFLOWS, "opencode-review-reusable.yml")
NIGHTSHIFT = os.path.join(WORKFLOWS, "stellar-nightshift.yml")
REVIEW_SCRIPTS_TEST = os.path.join(WORKFLOWS, "review-scripts-test.yml")
SKILL = os.path.join(ROOT, ".agents", "skills", "stellar-review", "SKILL.md")
ADR_0302 = os.path.join(ROOT, "docs", "adr", "0302-review-model-env-override.md")
RESTORE_HELPER = os.path.join(ROOT, ".github", "scripts", "review-restore.sh")
REVIEW_ENDPOINTS = os.path.join(ROOT, ".github", "scripts", "lib", "review-endpoints.sh")
STELLAR_PREP = os.path.join(ROOT, ".github", "scripts", "stellar-prep.sh")
ENFORCE_VERDICT = os.path.join(ROOT, ".github", "scripts", "enforce-opencode-verdict.sh")
FACTS = os.path.join(ROOT, ".agents", "skills", "stellar-review", "tools", "facts.py")
INCREMENTAL = os.path.join(ROOT, ".agents", "skills", "stellar-review", "tools", "incremental.py")

# Never re-typed: the four pass-tier names are exactly the ones the
# generator reads (pass_models.pass_model_env()), plus the run-level
# override every review shares.
PASS_TIER_NAMES = tuple(pass_models.pass_model_env().values())
SECRET_NAMES = ("OPENCODE_REVIEW_MODEL",) + PASS_TIER_NAMES

# A whole STELLAR_MODEL_* token: `\w+` runs to the end of the identifier, so
# a renamed name (`STELLAR_MODEL_STRUCTURE_`) is never truncated back into a
# registered one.
MODEL_TOKEN = re.compile(r"\bSTELLAR_MODEL_\w+")

# Each review run and the env var its `opencode run --model` reads; the
# var's env line carries the default pin.
MODEL_RUNS = (
    (REUSABLE, "OPENCODE_REVIEW_MODEL"),
    (NIGHTSHIFT, "STELLAR_NIGHTSHIFT_MODEL"),
)

# review-scripts-test.yml must run when any of these change, or this guard
# never executes in CI. REVIEW_GLOB is also the caller set's source.
REVIEW_GLOB = ".github/workflows/opencode-review-*.yml"
WATCHED_PATHS = (
    REVIEW_GLOB,
    ".github/workflows/stellar-nightshift.yml",
    ".agents/skills/stellar-review/SKILL.md",
)

# The reusable workflow's callers: every watched review workflow but the
# reusable one itself, so a new caller is guarded without being listed.
CALLERS = tuple(
    sorted(
        path
        for path in glob.glob(os.path.join(ROOT, REVIEW_GLOB))
        if path != REUSABLE
    )
)

# The complete set of process-start code-injection channels every
# token-bearing post step pins to the empty string — every env var the
# ELF loader and the runner's interpreters document for loading code
# at process start: the shell startup files (BASH_ENV — bash's
# non-interactive startup file; ENV — the POSIX sh counterpart), the
# ELF dynamic loader's channels (LD_PRELOAD — a library whose
# constructors run at process start, LD_LIBRARY_PATH — the search
# path that resolves every later dependency, LD_AUDIT — an audit
# object loaded into every process, ld.so(8)), python's startup file
# and search paths (PYTHONSTARTUP, PYTHONPATH — the module search
# path that shadows stdlib imports, PYTHONHOME — the relocated
# standard library itself, python(1)), node's per-process flags and
# module path (NODE_OPTIONS — flags may carry --require, NODE_PATH —
# the module search path, node(1)), perl's injected switches and
# module paths (PERL5OPT — switches may carry -M, PERL5LIB, PERLLIB —
# the @INC search paths, perlrun(1)), ruby's injected options and
# load path (RUBYOPT — options may carry -r, RUBYLIB — the $LOAD_PATH
# entries, ruby(1)), and RubyGems' gem-resolution paths (GEM_HOME,
# GEM_PATH — gem(1)). An empty value is inert for every one of them —
# an empty list has no entries and an empty startup-file name opens
# nothing — so no step behavior changes. PATH is pinned separately,
# to the fixed system list review-restore.sh pins.
PINNED_STARTUP_VARS = (
    "BASH_ENV",
    "ENV",
    "LD_PRELOAD",
    "LD_LIBRARY_PATH",
    "LD_AUDIT",
    "PYTHONSTARTUP",
    "PYTHONPATH",
    "PYTHONHOME",
    "NODE_OPTIONS",
    "NODE_PATH",
    "PERL5OPT",
    "PERL5LIB",
    "PERLLIB",
    "RUBYOPT",
    "RUBYLIB",
    "GEM_HOME",
    "GEM_PATH",
)

# The git configuration sources every token-bearing post step pins away
# (the round-5 finding: the model shell can write .git/config, the
# worktree-scoped config and the shared HOME gitconfig, and every git
# command the post steps run parses them): the global (shared HOME) and
# system gitconfigs point at /dev/null, the system file is skipped
# outright, and the env-numbered config channel is zeroed —
# GIT_CONFIG_KEY_n/GIT_CONFIG_VALUE_n pairs are git configuration too,
# plantable through the runner env file under RUNNER_TEMP. The values
# are the workflow's step env spellings; review-restore.sh exports the
# same pins in-script (defense-in-depth, the PATH-pin precedent).
PINNED_GIT_CONFIG = {
    "GIT_CONFIG_GLOBAL": "/dev/null",
    "GIT_CONFIG_SYSTEM": "/dev/null",
    "GIT_CONFIG_NOSYSTEM": "'1'",
    "GIT_CONFIG_COUNT": "'0'",
}

# The full per-invocation git pin set every restore-path and fetch git
# invocation carries (the round-6 finding, extended by round 9): the
# round-5 fsmonitor/pager pair plus core.worktree=. (git's setup
# ignores command-line core.worktree — it is belt; the .git/config
# replacement is the load-bearing core.worktree defense),
# protocol.ext.allow=never (an insteadOf rewrite can never reach an
# ext:: fetch), core.attributesFile= (a model-written attributes file
# binds no filter) and core.hooksPath=/dev/null (the round-9 finding:
# a model-crafted .git/hooks script must never execute — the pin
# redirects every hook lookup to /dev/null, where none is found). The
# workflow's three staging git shows carry the same string, pinned
# byte-for-byte by the staging-literal assertion below.
GIT_PIN_SET = (
    "-c core.fsmonitor= -c core.pager=cat -c core.worktree=. "
    "-c protocol.ext.allow=never -c core.attributesFile= "
    "-c core.hooksPath=/dev/null"
)

# Where the pin set must appear, and the minimum number of git
# invocations in each file that carry it: the helper's reset, clean and
# ls-files; the endpoint library's git_authed wrapper — the one
# authenticated git invocation, so both fetches of fetch_review_base
# and stellar-prep's CI-arm LAST fetch (which routes through it, pinned
# by the credential-channel guard in test_review_git_hardening.py)
# carry the set; the enforce gate's tracked-verdict ls-files. The
# workflow's three staging git shows carry the same string, pinned
# byte-for-byte by the staging-literal assertion below.
PINNED_GIT_CALL_SITES = (
    (RESTORE_HELPER, 3),
    (REVIEW_ENDPOINTS, 1),
    (ENFORCE_VERDICT, 1),
)

# The replace-objects kill switch (the round-8 finding): the model step
# runs with shell access and can create refs/replace/<SHA> entries, and
# git resolves every SHA through a replacement by default — a
# model-created refs/replace/<HEAD_SHA> would steer the staging git
# show onto attacker-crafted helper bytes, and a replacement of the
# merge SHA would steer the reset onto an attacker-crafted tree.
# Disabled three ways, one posture: the staging git shows and the
# helper's reset carry the flag on the command line (pinned by the
# staging-literal assertion and the test below),
# review-restore.sh exports the env var before its first git call
# (every git call the sourced choreography runs), and every
# token-bearing post step pins the env var from process start —
# covering the git calls that run before the choreography is sourced
# (the staging show) and after it returns (the prep fetches,
# merge-base resolutions, facts recomputation, the enforce ls-files).
GIT_NO_REPLACE_FLAG = "--no-replace-objects"
GIT_NO_REPLACE_ENV = {"GIT_NO_REPLACE_OBJECTS": "'1'"}

# The two flags every facts-recomputation git diff carries (the
# round-8 finding): --no-ext-diff refuses external diff drivers
# (diff.external and diff.<driver>.command alike), --no-textconv
# refuses textconv filters. facts.py and incremental.py recompute the
# diff artifacts inside the token-bearing post steps, where a
# model-written driver in .git/config — planted during the model step
# or re-added by a persistent model-spawned child between git calls —
# would otherwise execute during the diff. One spelling: the literal
# flags at every diff invocation site, directly after the "diff"
# subcommand argument (the regex tolerates line wrapping between the
# arguments), held by the drift test below.
FACTS_DIFF_PINS_RE = re.compile(
    r'"diff",\s*"--no-ext-diff",\s*"--no-textconv"'
)

# Where the facts-recomputation diffs live, and the minimum number of
# pinned diff invocations in each: facts.py's per-file hunks, numstat,
# name-status and full-diff reads; incremental.py's anchoring patch.
FACTS_DIFF_SITES = (
    (FACTS, 4),
    (INCREMENTAL, 1),
)

# The ordered deny policies the Run OpenCode step carries through
# OPENCODE_CONFIG_CONTENT (the round-6 finding): passwordless sudo and
# docker (the runner-root tamper channel), the network-client commands
# and git's network verbs plus bash's /dev/tcp redirections (the
# provider-key exfil channel), and tool writes to absolute paths —
# with the RUNNER_TEMP carve-out LAST, because the last policy
# matching a resource decides: the _temp allow must stay after the
# edit deny or the carve-out stops working. Any change to this list
# must re-run the docker transcript verification the round-6 node
# recorded (pinned opencode 2.0.12, --auto, each policy refused).
MODEL_DENY_POLICIES = (
    ("permission", "shell:sudo *", "deny"),
    ("permission", "shell:docker *", "deny"),
    ("permission", "shell:curl *", "deny"),
    ("permission", "shell:wget *", "deny"),
    ("permission", "shell:nc *", "deny"),
    ("permission", "shell:ncat *", "deny"),
    ("permission", "shell:socat *", "deny"),
    ("permission", "shell:ssh *", "deny"),
    ("permission", "shell:scp *", "deny"),
    ("permission", "shell:sftp *", "deny"),
    ("permission", "shell:ftp *", "deny"),
    ("permission", "shell:telnet *", "deny"),
    ("permission", "shell:rsync *", "deny"),
    ("permission", "shell:git clone *", "deny"),
    ("permission", "shell:git fetch *", "deny"),
    ("permission", "shell:git pull *", "deny"),
    ("permission", "shell:git push *", "deny"),
    ("permission", "shell:git ls-remote *", "deny"),
    ("permission", "shell:git submodule *", "deny"),
    ("permission", "shell:*> /dev/tcp/*", "deny"),
    ("permission", "shell:*<>/dev/tcp/*", "deny"),
    ("permission", "edit:/*", "deny"),
    ("permission", "edit:/home/runner/work/_temp/*", "allow"),
)


def mapping_entry(region, name):
    """The lines of the `name:` entry inside a YAML mapping region, up to
    the next key at the same indentation (or the end of the region)."""
    m = re.search(r"^( *)" + re.escape(name) + r": *$", region, re.M)
    if m is None:
        return None
    rest = region[m.end():]
    nxt = re.search(r"^ {" + str(len(m.group(1))) + r"}\S", rest, re.M)
    return rest[: nxt.start()] if nxt else rest


def env_default(text, name):
    """The `|| '<literal>'` fallback on `name`'s env line."""
    m = re.search(r"(?m)^ *" + re.escape(name) + r":.*\|\| *'([^']+)'", text)
    if m is None:
        raise AssertionError(f"{name}: no `|| '<default>'` fallback on its env line")
    return m.group(1)


def env_value(step, name):
    """The value on `name`'s env line inside `step`, or None when absent."""
    m = re.search(r"^ *" + re.escape(name) + r": *(.*)$", step, re.M)
    return m.group(1) if m else None


def env_block_value(step, name):
    """The dedented text of `name`'s literal block scalar (`|-`) inside
    `step` — every following line more indented than the `name:` key —
    or None when absent. The block form the Run OpenCode step uses for
    OPENCODE_CONFIG_CONTENT."""
    lines = step.splitlines()
    for i, line in enumerate(lines):
        m = re.match(r"^( *)" + re.escape(name) + r": *\|-$", line)
        if not m:
            continue
        indent = len(m.group(1))
        block = []
        for follow in lines[i + 1:]:
            if follow.strip() == "":
                block.append("")
                continue
            if len(follow) - len(follow.lstrip(" ")) <= indent:
                break
            block.append(follow[indent + 2:])
        return "\n".join(block)
    return None


def secrets_block(path):
    """`path`'s text after its one `secrets:` line; fails unless the file
    has exactly one."""
    text = read(path)
    starts = list(re.finditer(r"^ *secrets: *$", text, re.M))
    if len(starts) != 1:
        raise AssertionError(
            f"{path}: expected exactly one `secrets:` block, found {len(starts)}"
        )
    return text[starts[0].end():]


def model_step():
    """The reusable workflow's Run OpenCode step (the shared model run)."""
    return region_between(
        read(REUSABLE), r"^ *- name: Run OpenCode", r"^ *- name: ", REUSABLE
    )


def pass_model_step():
    """The reusable workflow's stellar-only step that turns the pass
    overrides into OpenCode agent config (pass_models.py)."""
    return region_between(
        read(REUSABLE),
        r"^ *- name: Apply stellar pass-model overrides",
        r"^ *- name: ",
        REUSABLE,
    )


def post_model_steps():
    """(name, region) of every step after the Run OpenCode step, in
    workflow order — the steps the shell-capable model step precedes.
    The step-entry indentation is taken from the model step's own
    line, so deeper `- name:`-shaped text inside a step never splits
    the enumeration."""
    text = read(REUSABLE)
    m = re.search(r"^( *- name:) Run OpenCode", text, re.M)
    if m is None:
        raise AssertionError(f"{REUSABLE}: no Run OpenCode step found")
    rest = text[m.start():]
    starts = list(re.finditer(r"^" + re.escape(m.group(1)) + r" ", rest, re.M))
    prefix = len(m.group(1)) + 1
    steps = []
    for i, s in enumerate(starts):
        end = starts[i + 1].start() if i + 1 < len(starts) else len(rest)
        block = rest[s.start():end]
        steps.append((block.split("\n", 1)[0][prefix:].rstrip(), block))
    return steps[1:]  # starts[0] is the model step itself


def token_bearing_post_steps():
    """(name, region) of the post-model steps whose env maps GH_TOKEN —
    the workflow's marker for a step that authenticates to GitHub as
    CI, i.e. every step the process-start pinning must cover. The set
    is derived from the workflow, never hand-listed, so a step added
    there fails the pin test until it carries the full pin list."""
    steps = [
        (name, region)
        for name, region in post_model_steps()
        if re.search(r"^ *GH_TOKEN: ", region, re.M)
    ]
    if not steps:
        raise AssertionError(
            f"{REUSABLE}: no post-model step maps GH_TOKEN — the "
            "token-bearing step derivation found nothing to guard"
        )
    return steps


def restore_pinned_path():
    """The fixed system PATH review-restore.sh pins — the one spelling
    the workflow's step-level PATH pins must match."""
    m = re.search(r"(?m)^export PATH=(\S+)$", read(RESTORE_HELPER))
    if m is None:
        raise AssertionError(f"{RESTORE_HELPER}: no `export PATH=` pin found")
    return m.group(1)


def model_tiers_rule():
    """SKILL.md's MODEL TIERS rule, through the next `## ` heading."""
    return region_between(read(SKILL), r"^#* *MODEL TIERS", r"^## ", SKILL)


class TestModelEnvDrift(unittest.TestCase):
    def assert_forwarded(self, step, label, name):
        value = env_value(step, name)
        self.assertIsNotNone(value, f"{REUSABLE}: {name} missing from the {label} env")
        self.assertIn(
            f"secrets.{name}",
            value,
            f"{REUSABLE}: {name} in the {label} env must read the secret",
        )
        self.assertIn(
            f"vars.{name}",
            value,
            f"{REUSABLE}: {name} in the {label} env must fall back to the repo variable",
        )

    def test_model_step_env_forwards_only_the_run_override(self):
        step = model_step()
        self.assert_forwarded(step, "Run OpenCode step", "OPENCODE_REVIEW_MODEL")
        self.assertEqual(
            MODEL_TOKEN.findall(step),
            [],
            f"{REUSABLE}: the Run OpenCode step must carry no STELLAR_MODEL_* "
            "name — pass overrides apply through the pass-model step's "
            "OpenCode config, never the shared model step env",
        )

    def test_model_step_env_blanks_the_publish_channels(self):
        # A shell-capable model must not publish environment variables or
        # PATH entries into later steps: with GITHUB_ENV/GITHUB_PATH set to
        # the empty string, the runner's env/PATH files are replaced by ''
        # — not a writable path — so every publish attempt fails and no
        # BASH_ENV-style payload is live in the post steps. PROMPT_FILE,
        # published as a fixed value by the earlier prompt step, still
        # reaches the model step as a normal env var.
        step = model_step()
        for name in ("GITHUB_ENV", "GITHUB_PATH"):
            with self.subTest(name=name):
                self.assertEqual(
                    env_value(step, name),
                    "''",
                    f"{REUSABLE}: the Run OpenCode step env must set {name} "
                    "to the empty string — the model shell must not publish "
                    "env or PATH entries into later steps",
                )
        self.assertIn(
            '"$PROMPT_FILE"',
            step,
            f"{REUSABLE}: the Run OpenCode step still reads $PROMPT_FILE — "
            "the fixed-value publish from the prompt-file step must keep "
            "reaching the model step as a normal env var",
        )

    def test_restore_call_sites_stage_the_helper_from_trusted_content(self):
        # The three hardening call sites (scripted post, per-kind post,
        # enforce) must never source the worktree copy of
        # review-restore.sh — the model step could have rewritten it
        # before any token-bearing step runs. Each stages the committed
        # PR-head copy (git show of the event payload's head SHA) into a
        # fresh mktemp file and sources that; the staging show carries
        # the full per-invocation pin set — --no-replace-objects (a
        # model-created refs/replace entry cannot steer which object
        # the show reads) plus the config pins — so a model-written
        # .git/config cannot steer even this first git call (the step
        # env pins the file sources, but the repository config still
        # parses until the sourced choreography replaces it); the
        # helper then resets to RESTORE_SHA — the event payload's
        # merge SHA, never the model-moveable local HEAD — after
        # replacing .git/config with the minimal trusted
        # configuration, whose origin URL it rebuilds from the REPO
        # every call site must pass.
        text = read(REUSABLE)
        self.assertNotIn(
            ". .github/scripts/review-restore.sh",
            text,
            f"{REUSABLE}: a restore call site sources the worktree copy of "
            "review-restore.sh — it must stage the committed PR-head copy "
            "via git show instead (ADR-0603)",
        )
        staging = (
            f'git {GIT_NO_REPLACE_FLAG} {GIT_PIN_SET} show '
            '"$HEAD_SHA:.github/scripts/review-restore.sh" > "$RESTORE"'
        )
        self.assertEqual(
            text.count(staging),
            3,
            f"{REUSABLE}: expected the three restore call sites to stage "
            "review-restore.sh with the pinned git show of the PR head SHA",
        )
        self.assertEqual(
            text.count('. "$RESTORE"'),
            3,
            f"{REUSABLE}: expected the three restore call sites to source "
            "the staged trusted copy",
        )
        self.assertEqual(
            text.count("RESTORE_SHA: ${{ github.sha }}"),
            3,
            f"{REUSABLE}: each restore call site must pass the event "
            "payload's merge SHA as RESTORE_SHA — the trusted reset target",
        )
        for name, step in token_bearing_post_steps():
            with self.subTest(step=name):
                self.assertEqual(
                    env_value(step, "REPO"),
                    "${{ github.repository }}",
                    f"{REUSABLE}: the '{name}' step must pass the "
                    "workflow-expression owner/name as REPO — the trusted "
                    "source the helper's minimal replacement .git/config "
                    "rebuilds the origin URL from",
                )

    def test_post_steps_pin_process_start_env(self):
        # The blanked GITHUB_ENV/GITHUB_PATH stop the model's own
        # file-command writes, but the runner's env file for each later
        # step is still writable by path under RUNNER_TEMP — and the
        # restore script's PATH pin only lands once the run block is
        # already executing, after bash/loader startup consumed
        # BASH_ENV/LD_PRELOAD/…. A step env entry is the most specific
        # Actions env scope (workflow syntax, jobs.<job_id>.steps.env:
        # an env variable defined in a step "will override job and
        # workflow environment variables with the same name, while the
        # step executes"), so pinning there overrides anything the model
        # published — from process start, with fixed literals only.
        # The guarded steps are not hand-listed: they are enumerated
        # from the workflow itself (every post-model step whose env
        # maps GH_TOKEN), so a step added or a pin renamed in the
        # workflow fails here until every token-bearing post step
        # carries the full list.
        path = restore_pinned_path()
        for name, step in token_bearing_post_steps():
            for var in PINNED_STARTUP_VARS:
                with self.subTest(step=name, var=var):
                    self.assertEqual(
                        env_value(step, var),
                        "''",
                        f"{REUSABLE}: the '{name}' step must pin {var} to "
                        "the empty string — a model-published process-start "
                        "channel must be inert from process start",
                    )
            with self.subTest(step=name, var="PATH"):
                self.assertEqual(
                    env_value(step, "PATH"),
                    path,
                    f"{REUSABLE}: the '{name}' step must pin PATH to the "
                    "fixed system list review-restore.sh pins — the "
                    "workflow-fixed value, never model-published content",
                )

    def test_post_steps_pin_git_config_sources(self):
        # Model-written git configuration must be inert in every git
        # call a token-bearing post step runs (the round-5 finding):
        # the model shell can write the shared HOME gitconfig during
        # the model step, and GIT_CONFIG_KEY_n/GIT_CONFIG_VALUE_n pairs
        # planted through the runner env file under RUNNER_TEMP are
        # parsed as git configuration by every git command. The step
        # env pins the file sources from process start — before the
        # staging git show runs. The repository's own .git/config
        # survives the pin and is handled twice over instead: the
        # sourced review-restore.sh replaces it with the minimal
        # trusted configuration before its first git invocation, and
        # the staging git show plus the helper's own invocations carry
        # the full six-flag GIT_PIN_SET per invocation. The guarded
        # steps are the same workflow-derived token-bearing set the
        # process-start pin test above iterates.
        for name, step in token_bearing_post_steps():
            for var, value in PINNED_GIT_CONFIG.items():
                with self.subTest(step=name, var=var):
                    self.assertEqual(
                        env_value(step, var),
                        value,
                        f"{REUSABLE}: the '{name}' step must pin {var} "
                        f"to {value} — model-written git configuration "
                        "must be inert in every git call the step runs",
                    )

    def test_restore_helper_exports_the_same_config_pins(self):
        # review-restore.sh re-exports the step env's config-source
        # pins in-script — the PATH-pin defense-in-depth precedent: the
        # sourced choreography must hold in any shell it is sourced
        # into, and the two spellings must never drift apart.
        for var, value in PINNED_GIT_CONFIG.items():
            expected = value.strip("'")
            with self.subTest(var=var):
                m = re.search(
                    r"(?m)^export " + re.escape(var) + r"=(\S+)$",
                    read(RESTORE_HELPER),
                )
                self.assertIsNotNone(
                    m,
                    f"{RESTORE_HELPER}: no `export {var}=` pin found — "
                    "the choreography must re-export the workflow's "
                    "config-source pins",
                )
                self.assertEqual(
                    m.group(1),
                    expected,
                    f"{RESTORE_HELPER}: the {var} pin drifted from the "
                    f"workflow's step env value ({expected})",
                )

    def test_pinned_git_invocations_carry_the_full_pin_set(self):
        # Every git invocation of the restore path and the fetches
        # carries the full six-flag pin set (the round-6 finding,
        # extended by round 9): the repository's own .git/config still
        # parses in every shell the post steps run before
        # review-restore.sh's replacement lands (the staging git show),
        # and the fetches re-parse it after the model step — the pin
        # set holds the worktree/filter/transport/hooks knobs closed
        # even where the replacement has not run. The minimum counts
        # are the git invocations each file owns: the helper's reset,
        # clean and ls-files; the endpoint library's git_authed wrapper
        # (fetch_review_base's two fetches and stellar-prep's CI-arm
        # last_head fetch route through it); the enforce gate's
        # tracked-verdict ls-files.
        for path, minimum in PINNED_GIT_CALL_SITES:
            with self.subTest(file=os.path.basename(path)):
                self.assertGreaterEqual(
                    read(path).count(GIT_PIN_SET),
                    minimum,
                    f"{path}: fewer than {minimum} git invocations carry "
                    "the full per-invocation pin set — every restore-path "
                    "and fetch git call must pin the repository config's "
                    "worktree/filter/transport/hooks knobs off",
                )

    def test_replace_objects_are_disabled_in_every_token_bearing_step(self):
        # The round-8 finding: the model step runs with shell access and
        # can create refs/replace/<SHA> entries; git resolves every SHA
        # through a replacement by default, so a model-created
        # refs/replace/<HEAD_SHA> would steer the staging git show onto
        # attacker-crafted helper bytes and the reset onto an
        # attacker-crafted tree. Three pins, one posture, all pinned
        # here: the three staging shows carry --no-replace-objects (the
        # staging-literal assertion above); the helper's reset carries
        # it on the command line; review-restore.sh exports
        # GIT_NO_REPLACE_OBJECTS=1 before its first git call (the reset
        # — so every git call the sourced choreography runs resolves
        # true objects); and every token-bearing post step pins the env
        # var from process start, covering the git calls that run
        # before the choreography is sourced and after it returns.
        helper = read(RESTORE_HELPER)
        reset = re.search(
            r"(?m)^git " + re.escape(GIT_NO_REPLACE_FLAG) + r" \S.* reset --hard ",
            helper,
        )
        self.assertIsNotNone(
            reset,
            f"{RESTORE_HELPER}: the reset must carry "
            f"{GIT_NO_REPLACE_FLAG} — it resolves RESTORE_SHA, and a "
            "model-created refs/replace entry must not steer which "
            "tree it lands on",
        )
        export = helper.find("export GIT_NO_REPLACE_OBJECTS=1")
        self.assertGreaterEqual(
            export,
            0,
            f"{RESTORE_HELPER}: no `export GIT_NO_REPLACE_OBJECTS=1` — "
            "the sourced choreography must hold replace objects off "
            "for every git call it runs",
        )
        self.assertLess(
            export,
            reset.start(),
            f"{RESTORE_HELPER}: the GIT_NO_REPLACE_OBJECTS export must "
            "land before the choreography's first git invocation — "
            "the reset",
        )
        for name, step in token_bearing_post_steps():
            for var, value in GIT_NO_REPLACE_ENV.items():
                with self.subTest(step=name, var=var):
                    self.assertEqual(
                        env_value(step, var),
                        value,
                        f"{REUSABLE}: the '{name}' step must pin {var} "
                        f"to {value} — a model-created refs/replace "
                        "entry must not steer any SHA-resolving git "
                        "call the step runs",
                    )

    def test_facts_recomputation_diffs_refuse_external_drivers(self):
        # The round-8 finding: the stellar and per-kind post steps
        # recompute the facts artifacts — facts.py's four diff reads,
        # and the stellar lane's incremental.py anchoring patch — after
        # the shell-capable model step, inside a token-bearing step. A
        # model-written diff.external or diff.<driver>.command in
        # .git/config, planted during the model step or re-added by a
        # persistent model-spawned child between git calls, would
        # execute during those diffs. Every diff invocation site in
        # both modules therefore carries --no-ext-diff --no-textconv:
        # the drivers and filters never run, whatever the repository
        # config says. The minimum counts pin the sites that exist
        # today; the finditer assertion holds the line honest — no
        # diff invocation may appear without the pins.
        for path, minimum in FACTS_DIFF_SITES:
            with self.subTest(file=os.path.basename(path)):
                text = read(path)
                self.assertGreaterEqual(
                    len(FACTS_DIFF_PINS_RE.findall(text)),
                    minimum,
                    f"{path}: fewer than {minimum} git diff invocations "
                    "carry --no-ext-diff --no-textconv — every "
                    "facts-recomputation diff must refuse external "
                    "drivers and textconv filters",
                )
                for m in re.finditer(r'"diff",', text):
                    self.assertIsNotNone(
                        FACTS_DIFF_PINS_RE.match(text, m.start()),
                        f"{path}: a git diff invocation at offset "
                        f"{m.start()} lacks the diff pins — every "
                        "diff invocation site must carry "
                        "--no-ext-diff --no-textconv",
                    )

    def test_model_step_keeps_auto_and_denies_the_named_channels(self):
        # The round-6 finding: --auto alone would auto-approve the
        # runner-root tamper channel (passwordless sudo, docker on the
        # hosted runners) and the provider-key exfil channel (the
        # network-client commands). The Run OpenCode step keeps --auto
        # (the round-4 permission-abort class — a declined cleanup once
        # killed the lane) but carries explicit deny policies through
        # OPENCODE_CONFIG_CONTENT, a virtual config layer the runner
        # authors: the flag auto-approves only what no deny rule
        # blocks, and a denied call fails the tool call, not the run.
        # They are experimental.policies, not plain permission rules,
        # because the policy hook forces its deny after rule
        # evaluation — a PR's own project config (an opencode.json in
        # the reviewed tree, the last config layer) cannot re-allow
        # what this layer denies. The _temp carve-out must stay LAST:
        # the last policy matching a resource decides.
        step = model_step()
        self.assertIn(
            "--auto",
            step,
            f"{REUSABLE}: the Run OpenCode step must keep --auto — the "
            "round-4 permission-abort class; the deny rules, not the "
            "flag's removal, close the round-6 channels",
        )
        content = env_block_value(step, "OPENCODE_CONFIG_CONTENT")
        self.assertIsNotNone(
            content,
            f"{REUSABLE}: the Run OpenCode step must set "
            "OPENCODE_CONFIG_CONTENT to a `|-` literal block — the deny "
            "policies are its configuration",
        )
        doc = json.loads(content)  # not valid JSON → the test fails loudly
        self.assertEqual(
            set(doc),
            {"experimental"},
            f"{REUSABLE}: OPENCODE_CONFIG_CONTENT must carry the deny "
            "policies alone — no model, provider or agent keys belong in "
            "the runner-authored layer",
        )
        expected = [
            dict(zip(("action", "resource", "effect"), row))
            for row in MODEL_DENY_POLICIES
        ]
        self.assertEqual(
            doc["experimental"]["policies"],
            expected,
            f"{REUSABLE}: the deny policy list drifted from "
            "MODEL_DENY_POLICIES — any change must re-run the docker "
            "transcript verification on the pinned opencode",
        )

    def test_pass_model_step_env_forwards_every_pass(self):
        step = pass_model_step()
        for name in PASS_TIER_NAMES:
            with self.subTest(name=name):
                self.assert_forwarded(step, "pass-model step", name)

    def test_pass_model_step_configures_the_scripted_run(self):
        step = pass_model_step()
        self.assertRegex(
            step,
            r"(?m)^ *if: inputs\.pipeline == 'scripted' *$",
            f"{REUSABLE}: the pass-model step must run only for the scripted pipeline",
        )
        self.assertRegex(
            step,
            r"(?m)^ *run: python3 \.agents/skills/stellar-review/tools/pass_models\.py *$",
            f"{REUSABLE}: the pass-model step must run pass_models.py",
        )
        text = read(REUSABLE)
        self.assertLess(
            text.index(step),
            text.index(model_step()),
            f"{REUSABLE}: the pass-model step must run before the Run OpenCode step",
        )

    def test_no_unregistered_model_token(self):
        # Negative assertion: every STELLAR_MODEL_* token present must be a
        # registered pass tier — a renamed or an extra name fails.
        for label, region in ((REUSABLE, pass_model_step()), (SKILL, model_tiers_rule())):
            with self.subTest(surface=label):
                found = set(MODEL_TOKEN.findall(region))
                self.assertEqual(
                    found,
                    set(PASS_TIER_NAMES),
                    f"{label}: STELLAR_MODEL_* tokens drifted from "
                    f"pass_models.pass_model_env() (extra: "
                    f"{sorted(found - set(PASS_TIER_NAMES))}, missing: "
                    f"{sorted(set(PASS_TIER_NAMES) - found)})",
                )

    def test_default_pin_matches_adr_0302(self):
        pins = {var: env_default(read(path), var) for path, var in MODEL_RUNS}
        distinct = set(pins.values())
        self.assertEqual(
            len(distinct),
            1,
            f"the {' and '.join(pins)} default pins drifted apart: {pins}",
        )
        (pin,) = distinct
        decision = region_between(read(ADR_0302), r"^## Decision", r"^## ", ADR_0302)
        self.assertIn(
            pin,
            decision,
            f"{ADR_0302}: default {pin!r} is not in the Decision table — "
            "bumping the default requires a new ADR (accepted ADRs are "
            "immutable)",
        )

    def test_reusable_declares_the_workflow_call_secrets(self):
        rest = secrets_block(REUSABLE)
        jobs = re.search(r"^jobs:", rest, re.M)
        self.assertIsNotNone(
            jobs, f"{REUSABLE}: no `jobs:` after the workflow_call secrets map"
        )
        declared = rest[: jobs.start()]
        for name in SECRET_NAMES:
            with self.subTest(name=name):
                entry = mapping_entry(declared, name)
                self.assertIsNotNone(
                    entry,
                    f"{REUSABLE}: {name} not declared under on.workflow_call.secrets",
                )
                self.assertRegex(
                    entry,
                    r"(?m)^ *required: false *$",
                    f"{REUSABLE}: {name} must be declared required: false",
                )
        entry = mapping_entry(declared, "FIREWORKS_API_KEY")
        self.assertIsNotNone(
            entry, f"{REUSABLE}: FIREWORKS_API_KEY not declared under on.workflow_call.secrets"
        )
        self.assertRegex(
            entry,
            r"(?m)^ *required: true *$",
            f"{REUSABLE}: FIREWORKS_API_KEY must be declared required: true",
        )

    def test_callers_call_the_reusable_workflow(self):
        # A REVIEW_GLOB match that is not a caller must fail here, not be
        # held to (or silently skip) the secrets contract below.
        self.assertTrue(CALLERS, f"no caller workflow matches {REVIEW_GLOB}")
        uses = "./" + os.path.relpath(REUSABLE, ROOT)
        for path in CALLERS:
            with self.subTest(caller=os.path.basename(path)):
                self.assertRegex(
                    read(path),
                    r"(?m)^ *uses: *" + re.escape(uses) + r" *$",
                    f"{path}: matches {REVIEW_GLOB} but has no `uses: {uses}`",
                )

    def test_callers_pass_every_secret(self):
        for path in CALLERS:
            block = secrets_block(path)
            for name in SECRET_NAMES:
                with self.subTest(caller=os.path.basename(path), name=name):
                    pat = (
                        r"(?m)^ *"
                        + re.escape(name)
                        + r": *"
                        + re.escape("${{ secrets." + name + " }}")
                        + r" *$"
                    )
                    self.assertRegex(
                        block,
                        pat,
                        f"{path}: {name} is not passed as ${{{{ secrets.{name} }}}}",
                    )

    def test_review_runs_use_variable_models(self):
        for path, var in MODEL_RUNS:
            with self.subTest(workflow=os.path.basename(path)):
                tokens = re.findall(r"opencode run --model +(\S+)", read(path))
                self.assertTrue(tokens, f"{path}: no `opencode run --model` invocation")
                for token in tokens:
                    self.assertEqual(
                        token,
                        f'"${var}"',
                        f'{path}: `opencode run --model` must read "${var}"',
                    )

    def test_review_scripts_test_watches_the_guarded_files(self):
        paths = region_between(
            read(REVIEW_SCRIPTS_TEST),
            r"^ *pull_request:",
            r"^ *workflow_dispatch:",
            REVIEW_SCRIPTS_TEST,
        )
        for entry in WATCHED_PATHS:
            with self.subTest(path=entry):
                self.assertRegex(
                    paths,
                    r'(?m)^ *- "?' + re.escape(entry) + r'"? *$',
                    f"{REVIEW_SCRIPTS_TEST}: pull_request paths must include {entry}",
                )


if __name__ == "__main__":
    unittest.main()
