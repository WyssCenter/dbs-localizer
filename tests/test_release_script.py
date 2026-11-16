# SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
# SPDX-License-Identifier: Apache-2.0

"""scripts/release.py: the file edits and checks of a release (no git, no network)."""

from __future__ import annotations

from pathlib import Path

import pytest

from scripts import release

ROOT = Path(__file__).resolve().parent.parent

CARGO = """[workspace]
members = ["crates/a", "crates/b"]

[workspace.package]
version = "0.1.0"
license = "Apache-2.0"

[profile.release]
debug = false
"""
LOCK = """[[package]]
name = "a"
version = "0.1.0"

[[package]]
name = "ab"
version = "0.1.0"

[[package]]
name = "b"
version = "0.1.0"
dependencies = [
 "a",
]
"""
CITATION = 'cff-version: 1.2.0\nversion: "0.1.0"\nlicense: Apache-2.0\n'
CHANGELOG = "# Changelog\n\n## Unreleased\n\n- New thing.\n\n## 0.1.0 (2026-11-12)\n\nFirst.\n"
CITING = "  version = {0.1.0},\n"
CLI = (
    "curl https://github.com/WyssCenter/fastwarp/releases/download/v0.1.0/fastwarp-installer.sh\n"
    'gh release download v0.1.0 -R WyssCenter/fastwarp -p "x*"\n'
)


def tree() -> dict[str, str]:
    return {
        "Cargo.toml": CARGO,
        "Cargo.lock": LOCK,
        "CITATION.cff": CITATION,
        "CHANGELOG.md": CHANGELOG,
        "docs/citing.md": CITING,
        "docs/cli.md": CLI,
        "crates/a/Cargo.toml": '[package]\nname = "a"\nversion.workspace = true\n',
        "crates/b/Cargo.toml": '[package]\nname = "b"\nversion.workspace = true\n',
    }


def disk(path: str) -> str:
    return (ROOT / path).read_bytes().decode("utf-8")


@pytest.mark.parametrize(
    ("spec", "expected"),
    [("patch", "1.2.4"), ("minor", "1.3.0"), ("major", "2.0.0"), ("1.10.0", "1.10.0")],
)
def test_next_version(spec, expected):
    assert release.next_version("1.2.3", spec) == expected


@pytest.mark.parametrize(
    "spec", ["1.2.3", "1.2.2", "0.9.0", "1.3", "1.3.0-rc.1", "v1.3.0", "01.3.0", "1.03.0"]
)
def test_next_version_refuses(spec):
    with pytest.raises(release.ReleaseError):
        release.next_version("1.2.3", spec)


def test_release_edits_change_every_version():
    files = tree()
    edits = release.release_edits(files.__getitem__, "0.2.0", "2026-12-01")
    assert release.current_version(edits["Cargo.toml"]) == "0.2.0"
    assert 'license = "Apache-2.0"' in edits["Cargo.toml"]
    assert release.lock_versions(edits["Cargo.lock"], ["a", "b", "ab"]) == {
        "a": "0.2.0",
        "b": "0.2.0",
        "ab": "0.1.0",
    }
    assert edits["CITATION.cff"] == 'cff-version: 1.2.0\nversion: "0.2.0"\nlicense: Apache-2.0\n'
    assert edits["CHANGELOG.md"] == (
        "# Changelog\n\n## Unreleased\n\n## 0.2.0 (2026-12-01)\n\n- New thing.\n\n"
        "## 0.1.0 (2026-11-12)\n\nFirst.\n"
    )
    assert edits["docs/citing.md"] == "  version = {0.2.0},\n"
    assert "download/v0.2.0/" in edits["docs/cli.md"]
    assert "gh release download v0.2.0 -R" in edits["docs/cli.md"]
    assert "0.1.0" not in edits["docs/cli.md"]


def test_release_problems_before_and_after():
    files = tree()
    assert release.release_problems(files.__getitem__, "0.1.0") == []
    assert len(release.release_problems(files.__getitem__, "0.2.0")) == 7
    files.update(release.release_edits(files.__getitem__, "0.2.0", "2026-12-01"))
    assert release.release_problems(files.__getitem__, "0.2.0") == []


def test_roll_changelog_keeps_crlf():
    rolled = release.roll_changelog(CHANGELOG.replace("\n", "\r\n"), "0.2.0", "2026-12-01")
    assert "## Unreleased\r\n\r\n## 0.2.0 (2026-12-01)\r\n\r\n- New thing." in rolled
    assert "\n" not in rolled.replace("\r\n", "")


@pytest.mark.parametrize(
    "text",
    [
        "# Changelog\n\n## 0.1.0 (2026-11-12)\n\nFirst.\n",
        "# Changelog\n\n## Unreleased\n\n## 0.1.0 (2026-11-12)\n\nFirst.\n",
        "# Changelog\n\n## Unreleased\n\n- New.\n\n## 0.2.0 (2026-11-30)\n\nOld.\n",
    ],
)
def test_roll_changelog_refuses(text):
    with pytest.raises(release.ReleaseError):
        release.roll_changelog(text, "0.2.0", "2026-12-01")


def test_release_notes_and_section_lookup():
    assert release.release_notes(CHANGELOG, "Unreleased") == "- New thing."
    assert release.release_notes(CHANGELOG, "0.1.0") == "First."
    assert not release.has_changelog_section("## 0.1.01 (x)\n## 0.1.0.1\n## 0.1.0-rc.1\n", "0.1.0")


def test_set_cargo_version_needs_the_workspace_table():
    with pytest.raises(release.ReleaseError):
        release.set_cargo_version('[package]\nversion = "0.1.0"\n', "0.2.0")


def test_set_lock_versions_needs_every_package():
    with pytest.raises(release.ReleaseError):
        release.set_lock_versions(LOCK, ["a", "missing"], "0.2.0")


def test_pin_file_from_pypi():
    pypi = {
        "urls": [
            {
                "filename": "fastwarp-0.2.0.tar.gz",
                "packagetype": "sdist",
                "digests": {"sha256": "5d"},
            },
            {
                "filename": "fastwarp-0.2.0-win.whl",
                "packagetype": "bdist_wheel",
                "digests": {"sha256": "bb"},
            },
            {
                "filename": "fastwarp-0.2.0-linux.whl",
                "packagetype": "bdist_wheel",
                "digests": {"sha256": "aa"},
            },
        ]
    }
    wheels = release.wheel_hashes(pypi)
    assert release.render_pin("0.2.0", wheels) == (
        "fastwarp==0.2.0 \\\n    --hash=sha256:aa \\\n    --hash=sha256:bb\n"
    )
    sums = release.parse_sha256sums("aa  fastwarp-0.2.0-linux.whl\nBB *./fastwarp-0.2.0-win.whl\n")
    assert release.hash_mismatches(wheels, sums) == []
    assert len(release.hash_mismatches(wheels, {"fastwarp-0.2.0-linux.whl": "cc"})) == 2
    extra = {**sums, "fastwarp-0.2.0-mac.whl": "dd", "fastwarp-0.2.0.tar.gz": "5d"}
    assert release.hash_mismatches(wheels, extra) == [
        "fastwarp-0.2.0-mac.whl: PyPI none, GitHub release dd"
    ]
    with pytest.raises(release.ReleaseError):
        release.wheel_hashes({"urls": pypi["urls"][:1]})
    yanked = [{**entry, "yanked": True} for entry in pypi["urls"]]
    with pytest.raises(release.ReleaseError):
        release.wheel_hashes({"urls": yanked})


@pytest.mark.parametrize(
    "url",
    [
        "https://github.com/WyssCenter/fastwarp.git",
        "https://github.com/WyssCenter/fastwarp",
        "git@github.com:WyssCenter/fastwarp.git\n",
        "ssh://git@github.com/WyssCenter/fastwarp/",
    ],
)
def test_repository_slug(url):
    assert release.repository_slug(url) == "WyssCenter/fastwarp"


def test_repository_slug_refuses_other_hosts():
    with pytest.raises(release.ReleaseError):
        release.repository_slug("https://gitlab.com/WyssCenter/fastwarp.git")


def test_doc_edit_refuses_a_file_without_the_pattern():
    with pytest.raises(release.ReleaseError):
        release.set_doc_versions("docs/citing.md", "no version here\n", "0.2.0")


def released_trees() -> tuple[dict[str, str], dict[str, str]]:
    parent = tree()
    head = {**parent, **release.release_edits(parent.__getitem__, "0.2.0", "2026-12-01")}
    return parent, head


def tag_harness(monkeypatch, parent, head, typed="0.2.0"):
    sha = "a" * 40
    calls = []
    checked = []
    monkeypatch.setattr(release, "fetch_main", lambda: sha)
    monkeypatch.setattr(
        release,
        "reader_at",
        lambda ref: (parent if ref.endswith("~1") else head).__getitem__,
    )
    monkeypatch.setattr(release, "require_no_tag", lambda tag: None)
    monkeypatch.setattr(release, "require_ci", lambda commit: checked.append(("ci", commit)))
    monkeypatch.setattr(release, "require_dry_run", lambda commit: checked.append(("dry", commit)))
    monkeypatch.setattr(release, "show", lambda *args: calls.append(args))
    monkeypatch.setattr("builtins.input", lambda prompt="": typed)
    return sha, calls, checked


def test_tag_tags_the_checked_release_commit(monkeypatch):
    parent, head = released_trees()
    sha, calls, checked = tag_harness(monkeypatch, parent, head)
    assert release.main(["tag", "0.2.0"]) == 0
    assert checked == [("ci", sha), ("dry", sha)]
    assert calls == [
        ("git", "tag", "-a", "-m", "FastWarp 0.2.0", "v0.2.0", sha),
        ("git", "push", "origin", "refs/tags/v0.2.0"),
    ]


@pytest.mark.parametrize(
    ("on_remote", "deleted"), [(b"", True), (b"abc\trefs/tags/v0.2.0\n", False)]
)
def test_tag_push_failure_reports_where_the_tag_is(monkeypatch, capsys, on_remote, deleted):
    parent, head = released_trees()
    tag_harness(monkeypatch, parent, head)
    ran = []

    def show(*args):
        if args[:2] == ("git", "push"):
            raise release.ReleaseError("push refused")

    def run(*args, check=True):
        ran.append(args)
        return release.subprocess.CompletedProcess(args, 0, on_remote, b"")

    monkeypatch.setattr(release, "show", show)
    monkeypatch.setattr(release, "run", run)
    assert release.main(["tag"]) == 1
    assert (("git", "tag", "-d", "v0.2.0") in ran) is deleted
    error = capsys.readouterr().err
    assert "push refused" in error
    assert ("is not on origin" in error) is deleted


def test_tag_refuses_a_head_whose_parent_has_the_version(monkeypatch, capsys):
    _, head = released_trees()
    _, calls, checked = tag_harness(monkeypatch, head, head)
    assert release.main(["tag"]) == 1
    assert calls == [] and checked == []
    error = capsys.readouterr().err
    assert "not the release commit" in error and "Unreleased" not in error


def test_tag_needs_the_version_typed(monkeypatch):
    parent, head = released_trees()
    _, calls, _ = tag_harness(monkeypatch, parent, head, typed="y")
    assert release.main(["tag"]) == 0
    assert calls == []


def test_tag_refuses_commits_after_the_release(monkeypatch, capsys):
    _, head = released_trees()
    later = dict(head)
    later["CHANGELOG.md"] = head["CHANGELOG.md"].replace(
        "## Unreleased\n", "## Unreleased\n\n- Later.\n", 1
    )
    _, calls, checked = tag_harness(monkeypatch, head, later)
    assert release.main(["tag"]) == 1
    assert calls == [] and checked == []
    error = capsys.readouterr().err
    assert "Unreleased" in error and "not the release commit" in error


def test_tag_refuses_an_unmerged_release(monkeypatch):
    parent, _ = released_trees()
    _, calls, checked = tag_harness(monkeypatch, parent, parent)
    assert release.main(["tag", "0.2.0"]) == 1
    assert calls == [] and checked == []


def test_ci_counts_only_pushes_to_main(monkeypatch):
    def runs(event, branch, conclusion="success"):
        return (
            f'[{{"databaseId": 1, "status": "completed", "conclusion": "{conclusion}", '
            f'"event": "{event}", "headBranch": "{branch}", "url": ""}}]'
        )

    monkeypatch.setattr(release, "out", lambda *args: runs("push", "main"))
    release.require_ci("a" * 40)
    for listing in (
        runs("pull_request", "main"),
        runs("push", "release/0.2.0"),
        runs("push", "main", "cancelled"),
    ):
        monkeypatch.setattr(release, "out", lambda *args, listing=listing: listing)
        with pytest.raises(release.ReleaseError):
            release.require_ci("a" * 40)


def test_dry_run_counts_only_manual_runs(monkeypatch):
    listing = (
        '[{"databaseId": 1, "status": "completed", "conclusion": "success", '
        '"event": "push", "headBranch": "main", "url": ""}]'
    )
    monkeypatch.setattr(release, "out", lambda *args: listing)
    monkeypatch.setattr("builtins.input", lambda prompt="": "n")
    with pytest.raises(release.ReleaseError):
        release.require_dry_run("a" * 40)


def test_repository_files_match_the_release_edits():
    def with_entry(path: str) -> str:
        text = disk(path)
        if path == "CHANGELOG.md":
            text = text.replace("## Unreleased", "## Unreleased\n\n- Entry.\n", 1)
        return text

    version = release.current_version(disk("Cargo.toml"))
    bumped = release.next_version(version, "patch")
    edits = release.release_edits(with_entry, bumped, "2099-01-01")
    read = {**{path: disk(path) for path in edits}, **edits}
    for member in release.tomllib.loads(disk("Cargo.toml"))["workspace"]["members"]:
        read[f"{member}/Cargo.toml"] = disk(f"{member}/Cargo.toml")
    assert release.release_problems(read.__getitem__, bumped) == []


def test_repository_pin_file_has_the_rendered_layout():
    lines = disk(release.PIN_FILE).splitlines()
    version = lines[0].removeprefix("fastwarp==").removesuffix(" \\")
    hashes = [line.split("sha256:")[1].removesuffix(" \\") for line in lines[1:]]
    wheels = {f"{index:02d}": digest for index, digest in enumerate(hashes)}
    assert release.render_pin(version, wheels) == disk(release.PIN_FILE).replace("\r\n", "\n")
