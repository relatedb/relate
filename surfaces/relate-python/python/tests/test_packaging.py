"""Packaging assertions: the wheel (PEP 561 typing, the bundled library
skill) and the sdist (the skill survives maturin's staging walk)."""

import subprocess
import sys
import tarfile
from pathlib import Path, PurePosixPath

import relate


class TestLibrarySkill:
    def test_library_skill_ships_in_the_package(self):
        """The library-skills.io skill (relate/.agents/skills/relate/SKILL.md)
        is present in the installed package with Agent Skills frontmatter."""
        skill = (
            Path(relate.__file__).parent / ".agents" / "skills" / "relate" / "SKILL.md"
        )
        assert skill.is_file(), f"library skill missing: {skill}"
        text = skill.read_text()
        assert text.startswith("---")
        assert "name: relate" in text.split("---")[1]
        assert "import relate" in text

    def test_py_typed_marker_ships(self):
        """PEP 561 marker present so type checkers see the annotations."""
        marker = Path(relate.__file__).parent / "py.typed"
        assert marker.is_file(), f"py.typed missing: {marker}"


class TestSdist:
    def test_sdist_carries_the_library_skill(self, tmp_path):
        """The sdist ships the bundled library skill (LABS-1413).

        maturin 1.16's sdist staging walk silently drops dot-directories,
        so the staged tree — and the wheel the release's ``--sdist`` lane
        builds off that tree — shipped without ``python/relate/.agents``.
        The ``[tool.maturin] include`` re-adds it; this locks that in by
        building the sdist and asserting every skill file lands in it.
        """
        project_dir = Path(__file__).resolve().parents[2]
        skill_dir = project_dir / "python" / "relate" / ".agents"
        skill_files = sorted(
            path.relative_to(skill_dir)
            for path in skill_dir.rglob("*")
            if path.is_file()
        )
        assert skill_files, f"no library skill files under {skill_dir}"

        out_dir = tmp_path / "sdist"
        out_dir.mkdir()
        # `python -m maturin` resolves the one maturin the running
        # interpreter's venv owns — the dependency [build-system]
        # requires already declares — with no PATH probing; a missing
        # maturin fails loudly here instead of disarming the guard.
        subprocess.run(
            [sys.executable, "-m", "maturin", "sdist", "--out", str(out_dir)],
            cwd=project_dir,
            check=True,
        )
        (sdist,) = out_dir.glob("*.tar.gz")
        # Everything in the archive sits under one `<name>-<version>/`
        # root. The skill files must appear under that root at their
        # repo-relative location, compared in the archive's POSIX
        # namespace — enumerated, never extracted.
        with tarfile.open(sdist) as archive:
            members = {PurePosixPath(name) for name in archive.getnames()}
            roots = {member.parts[0] for member in members}
            assert len(roots) == 1, f"expected one archive root, got {sorted(roots)}"
            archive_root = PurePosixPath(roots.pop())
        # The skill's staged location derives from the one place its repo
        # location is already stated: skill_dir.
        staged_skill_dir = PurePosixPath(skill_dir.relative_to(project_dir))
        for relative in skill_files:
            assert (
                archive_root / staged_skill_dir / relative.as_posix() in members
            ), f"sdist dropped the library skill file {relative.as_posix()}"
