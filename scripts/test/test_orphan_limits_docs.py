"""Check that the real documents state issue #89's orphan-sweep platform limits.

Matrix row 89/AC-3 is the criterion "Platform limits (e.g. reading other
processes' env on macOS) are documented." Nothing executable can be asserted
about prose, so this reads the committed documents themselves: the limits are
the only thing telling a user which escaping processes the sweep will miss,
and silently dropping that paragraph would leave the row claiming coverage of
text that no longer exists.
"""

import pathlib
import re
import unittest

ROOT = pathlib.Path(__file__).resolve().parents[2]

# The exec-time environment limit, which every one of the four documents owes
# the reader. Each document words it differently (the execution README says
# "Linux `/proc`", README.md spells out `/proc/<pid>/environ`), so a fact is a
# regex over the prose rather than a fixed sentence.
ENVIRONMENT_LIMIT = {
    "linux reads /proc": r"/proc(/<pid>/environ)?",
    "macos reads KERN_PROCARGS2": r"KERN_PROCARGS2",
    "SIP hides Apple binaries": r"\bSIP\b",
    "process groups cover them": r"(process[ -]group|registered process groups?)",
}

# What the two user-facing documents additionally owe: every way a process
# escapes the sweep.
ESCAPES = {
    "sudo escapes": r"`?sudo`?",
    "other users escape": r"(another|other) users?'?s?\b",
    "group leavers escape": r"(setsid|setpgid|left the (step's )?group|group leavers)",
    # Both sentences wrap, so the gap has to cross newlines.
    "containers are never tagged": r"containers?[\s\S]{0,40}never tagged",
    "a crash skips the sweep": r"crash[\s\S]{0,80}skips the\s+sweep",
}

ENVIRONMENT_DOCS = [
    "README.md",
    "AGENTS.md",
    "crates/execution/src/execution/README.md",
    "docs/test-coverage.md",
]
ESCAPE_DOCS = ["README.md", "docs/test-coverage.md"]


def absent(text, facts):
    """The names in `facts` whose pattern matches nowhere in `text`."""
    return sorted(name for name, pattern in facts.items() if not re.search(pattern, text))


class OrphanLimitsDocsTest(unittest.TestCase):
    """Read the committed documents; no fixture stands in for them."""

    def test_every_document_states_the_exec_time_environment_limit(self):
        """README, AGENTS, the execution README and test-coverage each state it."""
        for name in ENVIRONMENT_DOCS:
            with self.subTest(document=name):
                text = (ROOT / name).read_text(encoding="utf-8")
                self.assertEqual(absent(text, ENVIRONMENT_LIMIT), [])

    def test_the_user_facing_documents_state_every_escape(self):
        """README and test-coverage name sudo, other users, group leavers,
        containers and a crash as processes the sweep does not reach."""
        for name in ESCAPE_DOCS:
            with self.subTest(document=name):
                text = (ROOT / name).read_text(encoding="utf-8")
                self.assertEqual(absent(text, ESCAPES), [])

    def test_each_fact_is_present_and_its_pattern_matches_only_itself(self):
        """Removing one fact from README must remove text and leave the other
        eight findable.

        The removal count is what proves the fact is in the prose at all. The
        exact-list comparison is what proves the pattern is specific to it: a
        pattern loose enough to match arbitrary prose would strip the document
        and report several facts missing, which would otherwise let both tests
        above pass without the documents saying anything.
        """
        text = (ROOT / "README.md").read_text(encoding="utf-8")
        facts = {**ENVIRONMENT_LIMIT, **ESCAPES}
        for name, pattern in facts.items():
            with self.subTest(removed=name):
                stripped, removed = re.subn(pattern, "", text)
                self.assertTrue(removed, "the pattern matched nothing to remove")
                self.assertEqual(absent(stripped, facts), [name])


if __name__ == "__main__":
    unittest.main()
