"""Drift guard for the review lanes' round-9 git-call hardening: hooks
neutralized through the config-replacement mechanism, and the fetch
credential delivered through git's env-numbered configuration channel
— never the command line. A focused guard module for the round-9
findings, following the test_last_head_fetch_guard precedent; the
round-5 through round-8 posture stays pinned by test_model_env_drift,
whose GIT_PIN_SET (extended by round 9 with -c core.hooksPath=/dev/null)
this module reuses — the one canonical pin-set spelling, never re-typed.

The round-9 High: the model step runs with shell access and can write
the default .git/hooks directory, which no round-5 through round-8 pin
covered — the round-7 GIT_CONFIG_GLOBAL=/dev/null pin had been masking
the round-6 hooks guarantee since it dropped the global hooksPath file.
Probed on git 2.43/2.54: an undefended `git reset --hard` executes a
model-crafted .git/hooks/reference-transaction (even a reset that lands
where the ref already points). The neutralization rides the
config-replacement mechanism itself: the minimal trusted .git/config
review-restore.sh writes carries core.hooksPath = /dev/null — every
hook lookup lands on /dev/null, where none is found — with the same
pin carried per invocation by GIT_PIN_SET on every token-bearing git
call (counted and byte-for-byte pinned by test_model_env_drift).

The round-9 Medium: the GitHub token rode the git fetch command line
(as an http extraheader config argument), readable by a surviving
model-spawned process through ps. The one authenticated git invocation
is git_authed, defined exactly once in the endpoint library: it
delivers the credential through the env-numbered channel — the count
and the key are fixed literals of the committed script, the value is
the auth argument the caller assembled from its step's GH_TOKEN, never
anything model-writable — and it carries the full per-invocation pin
set. stellar-prep's CI-arm LAST fetch routes through it, and no lane
surface spells an argv credential.

The channel is not itself a model-injection vector, and is reconciled
with the round-5 GIT_CONFIG_COUNT=0 pin (pinned by
test_model_env_drift's test_post_steps_pin_git_config_sources and
test_restore_helper_exports_the_same_config_pins): the channel never
widens beyond the command scope — the endpoint library and
stellar-prep never export GIT_CONFIG_COUNT — so the zero holds for
every other git call (a model-published GIT_CONFIG_KEY_n pair stays
inert), and the one command-scoped assignment raises the count to
exactly 1 — the single fixed extraheader entry — for the fetch
command alone. The two mechanisms cannot conflict.
"""

import os
import re
import unittest

from helpers import ROOT, read, region_between  # also puts tools/ on sys.path

# The one canonical pin-set spelling (extended by round 9) — reused,
# never re-typed here.
from test_model_env_drift import GIT_PIN_SET

REUSABLE = os.path.join(ROOT, ".github", "workflows", "opencode-review-reusable.yml")
RESTORE_HELPER = os.path.join(ROOT, ".github", "scripts", "review-restore.sh")
REVIEW_ENDPOINTS = os.path.join(ROOT, ".github", "scripts", "lib", "review-endpoints.sh")
STELLAR_PREP = os.path.join(ROOT, ".github", "scripts", "stellar-prep.sh")
KIND_PREP = os.path.join(ROOT, ".github", "scripts", "opencode-review-prep.sh")
ENFORCE_VERDICT = os.path.join(ROOT, ".github", "scripts", "enforce-opencode-verdict.sh")

# The env-numbered channel's fixed shape inside git_authed: the count
# and the key are literals of the committed script; the value is the
# auth argument (assembled from the step's GH_TOKEN by review_auth —
# trusted environment, never model-writable state).
GIT_AUTHED_CHANNEL = (
    "GIT_CONFIG_COUNT=1",
    "GIT_CONFIG_KEY_0='http.https://github.com/.extraheader'",
    'GIT_CONFIG_VALUE_0="$auth"',
)

# The lane surfaces a git credential may never ride the command line
# on: the restore helper, the endpoint library, both prep scripts, the
# enforce gate, and the reusable workflow itself.
ARGV_CREDENTIAL_SURFACES = (
    RESTORE_HELPER,
    REVIEW_ENDPOINTS,
    STELLAR_PREP,
    KIND_PREP,
    ENFORCE_VERDICT,
    REUSABLE,
)


class TestReviewGitHardening(unittest.TestCase):
    def test_replacement_config_pins_hooks_off(self):
        # The neutralization of the round-9 High rides the
        # config-replacement mechanism: the minimal trusted .git/config
        # review-restore.sh writes must carry core.hooksPath = /dev/null,
        # so the model-writable default .git/hooks directory is never
        # consulted — every hook lookup lands on /dev/null, where none
        # is found. (The per-invocation pin — GIT_PIN_SET's sixth flag —
        # is counted on every call site by test_model_env_drift, and the
        # behavioral payloads live in review-restore.bats and
        # review-endpoints.bats.)
        self.assertRegex(
            read(RESTORE_HELPER),
            r"(?m)^printf .*hooksPath = /dev/null",
            f"{RESTORE_HELPER}: the minimal trusted .git/config the "
            "replacement writes must pin core.hooksPath = /dev/null — "
            "the model-writable default .git/hooks directory must never "
            "be consulted (the round-9 finding)",
        )

    def test_authenticated_fetches_carry_no_credential_on_argv(self):
        # The round-9 Medium, pinned at the source level: git_authed is
        # defined exactly once in the endpoint library, delivers the
        # credential through the env-numbered channel with fixed count
        # and key literals, and carries the full per-invocation pin set.
        # stellar-prep's CI-arm LAST fetch routes through it, and no
        # lane surface spells an argv credential.
        text = read(REVIEW_ENDPOINTS)
        self.assertEqual(
            text.count("git_authed() {"),
            1,
            f"{REVIEW_ENDPOINTS}: git_authed must be defined exactly "
            "once — the one authenticated git invocation",
        )
        body = region_between(
            text, r"^git_authed\(\) \{", r"^\}", REVIEW_ENDPOINTS
        )
        for literal in GIT_AUTHED_CHANNEL:
            with self.subTest(literal=literal):
                self.assertIn(
                    literal,
                    body,
                    f"{REVIEW_ENDPOINTS}: git_authed's env-numbered "
                    f"credential channel drifted — the `{literal}` "
                    "spelling is fixed, with the count and key pinned "
                    "as literals of the committed script",
                )
        self.assertIn(
            GIT_PIN_SET,
            body,
            f"{REVIEW_ENDPOINTS}: git_authed must carry the full "
            "per-invocation pin set like every restore-path and fetch "
            "git call",
        )
        prep = region_between(
            read(STELLAR_PREP), r"^fetch_last_head\(\) \{", r"^\}", STELLAR_PREP
        )
        self.assertRegex(
            prep,
            r'git_authed "\$AUTH" fetch',
            f"{STELLAR_PREP}: the CI-arm LAST fetch must route through "
            "the endpoint library's git_authed — the one authenticated "
            "fetch spelling, off the command line",
        )
        for path in ARGV_CREDENTIAL_SURFACES:
            with self.subTest(surface=os.path.basename(path)):
                self.assertNotRegex(
                    read(path),
                    r"-c \S*extraheader",
                    f"{path}: a git credential rides the command line "
                    "(-c …extraheader) — ps-readable by a surviving "
                    "model-spawned process; deliver it through "
                    "git_authed's env-numbered channel instead",
                )

    def test_the_env_numbered_channel_never_widens_beyond_command_scope(self):
        # The channel is not itself a model-injection vector, and cannot
        # conflict with the round-5 GIT_CONFIG_COUNT=0 pin: the raiser
        # lives only as command-scoped assignments inside git_authed.
        # The endpoint library and stellar-prep never export
        # GIT_CONFIG_COUNT — an export would fight the step's
        # GIT_CONFIG_COUNT='0' pin instead of reconciling with it — and
        # the one spelling of the count-literal is the wrapper's own:
        # no lane surface but the endpoint library carries it.
        for path in (REVIEW_ENDPOINTS, STELLAR_PREP):
            with self.subTest(raiser=os.path.basename(path)):
                self.assertNotRegex(
                    read(path),
                    r"(?m)^export GIT_CONFIG_COUNT",
                    f"{path}: the env-numbered channel's raiser must "
                    "stay command-scoped inside git_authed — exporting "
                    "GIT_CONFIG_COUNT would fight the step's "
                    "GIT_CONFIG_COUNT='0' pin instead of reconciling "
                    "with it",
                )
        for path in (
            RESTORE_HELPER,
            STELLAR_PREP,
            KIND_PREP,
            ENFORCE_VERDICT,
            REUSABLE,
        ):
            with self.subTest(count_literal=os.path.basename(path)):
                self.assertNotIn(
                    "GIT_CONFIG_COUNT=1",
                    read(path),
                    f"{path}: a second env-numbered-channel raiser "
                    "outside git_authed — every authenticated fetch "
                    "must route through the endpoint library's one "
                    "spelling",
                )


if __name__ == "__main__":
    unittest.main()
