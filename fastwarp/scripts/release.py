# SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
# SPDX-License-Identifier: Apache-2.0

# /// script
# requires-python = ">=3.11"
# dependencies = []
# ///

"""Release helper for FastWarp maintainers (standard library only; needs git and gh).

Run it with uv, which picks the Python interpreter: uv run scripts/release.py COMMAND

prepare  bump the version, roll the changelog, open the release pull request
tag      check the merged release commit and its CI, then create and push the tag
finish   pin the benchmark to the published wheels and open a pull request

The steps are described in RELEASING.md.
"""

from __future__ import annotations

import argparse
import datetime
import difflib
import json
import os
import re
import shutil
import subprocess
import sys
import time
import tomllib
import urllib.error
import urllib.request
from collections.abc import Callable, Iterable
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
REMOTE = "origin"
MAIN = "main"
PYPI_PROJECT = "fastwarp"
PIN_FILE = "benchmarks/fastwarp-pypi.txt"
CI_WORKFLOW = "ci.yml"
RELEASE_WORKFLOW = "wheels.yml"

V = r"\d+\.\d+\.\d+"
DOC_EDITS = (
    ("docs/citing.md", rf"(version\s*=\s*\{{){V}(?=\}})"),
    ("docs/cli.md", rf"(releases/download/v){V}(?=/)"),
    ("docs/cli.md", rf"(gh release download v){V}(?![\w.])"),
)

Reader = Callable[[str], str]


class ReleaseError(Exception):
    pass


def parse_version(text: str) -> tuple[int, int, int]:
    match = re.fullmatch(r"(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)", text)
    if not match:
        raise ReleaseError(
            f"'{text}' is not a version of the form X.Y.Z; release candidates go through "
            "the manual TestPyPI run in RELEASING.md"
        )
    return tuple(int(part) for part in match.groups())


def next_version(current: str, spec: str) -> str:
    major, minor, patch = parse_version(current)
    bumps = {
        "major": (major + 1, 0, 0),
        "minor": (major, minor + 1, 0),
        "patch": (major, minor, patch + 1),
    }
    if spec in bumps:
        return ".".join(str(part) for part in bumps[spec])
    if parse_version(spec) <= (major, minor, patch):
        raise ReleaseError(f"{spec} is not newer than the current version {current}")
    return spec


def current_version(cargo_toml: str) -> str:
    try:
        return tomllib.loads(cargo_toml)["workspace"]["package"]["version"]
    except (KeyError, tomllib.TOMLDecodeError) as error:
        raise ReleaseError("Cargo.toml has no [workspace.package] version") from error


def _sub_once(pattern: str, replacement: str, text: str, what: str, flags: int = 0) -> str:
    result, count = re.subn(pattern, replacement, text, flags=flags)
    if count != 1:
        raise ReleaseError(f"expected one {what}, found {count}")
    return result


def set_cargo_version(text: str, version: str) -> str:
    return _sub_once(
        r'(^\[workspace\.package\][^\[]*?^version[ \t]*=[ \t]*")[^"]+(")',
        rf"\g<1>{version}\g<2>",
        text,
        "version in [workspace.package] of Cargo.toml",
        re.MULTILINE | re.DOTALL,
    )


def workspace_packages(read: Reader) -> list[str]:
    members = tomllib.loads(read("Cargo.toml"))["workspace"]["members"]
    return [tomllib.loads(read(f"{member}/Cargo.toml"))["package"]["name"] for member in members]


def set_lock_versions(text: str, packages: Iterable[str], version: str) -> str:
    for name in packages:
        text = _sub_once(
            rf'(^name = "{re.escape(name)}"\r?\nversion = ")[^"]+(")',
            rf"\g<1>{version}\g<2>",
            text,
            f"entry for {name} in Cargo.lock",
            re.MULTILINE,
        )
    return text


def lock_versions(text: str, packages: Iterable[str]) -> dict[str, str | None]:
    found = {}
    for name in packages:
        match = re.search(
            rf'^name = "{re.escape(name)}"\r?\nversion = "([^"]+)"', text, re.MULTILINE
        )
        found[name] = match.group(1) if match else None
    return found


def citation_version(text: str) -> str | None:
    match = re.search(r'^version:[ \t]*"([^"]+)"', text, re.MULTILINE)
    return match.group(1) if match else None


def set_citation_version(text: str, version: str) -> str:
    return _sub_once(
        r'(^version:[ \t]*")[^"]+(")',
        rf"\g<1>{version}\g<2>",
        text,
        "version in CITATION.cff",
        re.MULTILINE,
    )


def _section_end(text: str, start: int) -> int:
    following = re.search(r"^## ", text[start:], re.MULTILINE)
    return start + following.start() if following else len(text)


def has_changelog_section(text: str, version: str) -> bool:
    return re.search(rf"^## {re.escape(version)}(?!\S)", text, re.MULTILINE) is not None


def release_notes(text: str, heading: str) -> str:
    match = re.search(rf"^## {re.escape(heading)}(?!\S).*$", text, re.MULTILINE)
    if not match:
        raise ReleaseError(f"CHANGELOG.md has no '## {heading}' section")
    return text[match.end() : _section_end(text, match.end())].strip()


def unreleased_entries(text: str) -> str:
    match = re.search(r"^## Unreleased[ \t]*(?=\r?$)", text, re.MULTILINE)
    if not match:
        raise ReleaseError("CHANGELOG.md has no '## Unreleased' section")
    return text[match.end() : _section_end(text, match.end())].strip()


def roll_changelog(text: str, version: str, date: str) -> str:
    if has_changelog_section(text, version):
        raise ReleaseError(f"CHANGELOG.md already has a section for {version}")
    match = re.search(r"^## Unreleased[ \t]*(?=\r?$)", text, re.MULTILINE)
    if not match:
        raise ReleaseError("CHANGELOG.md has no '## Unreleased' section")
    if not text[match.end() : _section_end(text, match.end())].strip():
        raise ReleaseError("the '## Unreleased' section of CHANGELOG.md is empty")
    newline = "\r\n" if "\r\n" in text else "\n"
    heading = f"## Unreleased{newline}{newline}## {version} ({date})"
    return text[: match.start()] + heading + text[match.end() :]


def set_doc_versions(path: str, text: str, version: str) -> str:
    for edit_path, pattern in DOC_EDITS:
        if edit_path != path:
            continue
        text, count = re.subn(pattern, lambda match: match.group(1) + version, text)
        if count == 0:
            raise ReleaseError(f"{path} no longer matches the pattern {pattern}")
    return text


def doc_versions(path: str, text: str) -> set[str]:
    found = set()
    for edit_path, pattern in DOC_EDITS:
        if edit_path == path:
            found.update(
                match.group(0)[len(match.group(1)) :] for match in re.finditer(pattern, text)
            )
    return found


def release_edits(read: Reader, version: str, date: str) -> dict[str, str]:
    edits = {
        "Cargo.toml": set_cargo_version(read("Cargo.toml"), version),
        "Cargo.lock": set_lock_versions(read("Cargo.lock"), workspace_packages(read), version),
        "CITATION.cff": set_citation_version(read("CITATION.cff"), version),
        "CHANGELOG.md": roll_changelog(read("CHANGELOG.md"), version, date),
    }
    for path in dict.fromkeys(path for path, _ in DOC_EDITS):
        edits[path] = set_doc_versions(path, read(path), version)
    return edits


def release_problems(read: Reader, version: str) -> list[str]:
    problems = []
    cargo = current_version(read("Cargo.toml"))
    if cargo != version:
        problems.append(f"Cargo.toml has version {cargo}")
    for name, locked in lock_versions(read("Cargo.lock"), workspace_packages(read)).items():
        if locked != version:
            problems.append(f"Cargo.lock has {name} {locked}")
    cited = citation_version(read("CITATION.cff"))
    if cited != version:
        problems.append(f"CITATION.cff has version {cited}")
    if not has_changelog_section(read("CHANGELOG.md"), version):
        problems.append(f"CHANGELOG.md has no '## {version}' section")
    for path in dict.fromkeys(path for path, _ in DOC_EDITS):
        other = sorted(doc_versions(path, read(path)) - {version})
        if other:
            problems.append(f"{path} names {', '.join(other)}")
    return problems


def wheel_hashes(pypi: dict) -> dict[str, str]:
    wheels = {
        entry["filename"]: entry["digests"]["sha256"]
        for entry in pypi.get("urls", [])
        if entry.get("packagetype") == "bdist_wheel" and not entry.get("yanked")
    }
    if not wheels:
        raise ReleaseError("PyPI lists no wheels for this version")
    return wheels


def parse_sha256sums(text: str) -> dict[str, str]:
    sums = {}
    for line in text.splitlines():
        parts = line.split()
        if len(parts) == 2:
            sums[parts[1].lstrip("*").removeprefix("./")] = parts[0].lower()
    return sums


def hash_mismatches(wheels: dict[str, str], sums: dict[str, str]) -> list[str]:
    released = {name: digest for name, digest in sums.items() if name.endswith(".whl")}
    return [
        f"{name}: PyPI {wheels.get(name, 'none')}, GitHub release {released.get(name, 'none')}"
        for name in sorted(wheels.keys() | released.keys())
        if name not in wheels or released.get(name) != wheels[name].lower()
    ]


def repository_slug(url: str) -> str:
    match = re.search(r"github\.com[:/]([^/\s]+/[^/\s]+?)(?:\.git)?/?$", url.strip())
    if not match:
        raise ReleaseError(f"the remote {REMOTE} ({url.strip()}) is not a GitHub repository")
    return match.group(1)


def render_pin(version: str, wheels: dict[str, str]) -> str:
    lines = [f"{PYPI_PROJECT}=={version}"]
    lines += [f"    --hash=sha256:{wheels[name]}" for name in sorted(wheels)]
    return " \\\n".join(lines) + "\n"


def tool(name: str) -> str:
    path = shutil.which(name)
    if not path:
        raise ReleaseError(f"'{name}' is not on the PATH")
    return path


_slug: list[str] = []


def environment(name: str) -> dict[str, str] | None:
    if name != "gh":
        return None
    if not _slug:
        _slug.append(repository_slug(out("git", "remote", "get-url", REMOTE)))
    return {**os.environ, "GH_REPO": f"github.com/{_slug[0]}"}


def run(name: str, *args: str, check: bool = True) -> subprocess.CompletedProcess[bytes]:
    result = subprocess.run(
        [tool(name), *args], cwd=ROOT, capture_output=True, env=environment(name)
    )
    if check and result.returncode != 0:
        detail = (result.stderr or result.stdout).decode("utf-8", "replace").strip()
        raise ReleaseError(f"{name} {' '.join(args)} failed:\n{detail}")
    return result


def out(name: str, *args: str) -> str:
    return run(name, *args).stdout.decode("utf-8")


def show(name: str, *args: str) -> None:
    print(f"$ {name} {' '.join(args)}", flush=True)
    result = subprocess.run([tool(name), *args], cwd=ROOT, env=environment(name))
    if result.returncode != 0:
        raise ReleaseError(f"{name} {' '.join(args)} failed with status {result.returncode}")


def confirm(question: str, assume_yes: bool) -> bool:
    if assume_yes:
        print(f"{question} [yes]")
        return True
    return input(f"{question} [y/N] ").strip().lower() in ("y", "yes")


def reader_at(ref: str) -> Reader:
    return lambda path: out("git", "show", f"{ref}:{path}")


def write_files(edits: dict[str, str]) -> None:
    for path, text in edits.items():
        (ROOT / path).write_bytes(text.encode("utf-8"))


def print_diff(read: Reader, edits: dict[str, str]) -> None:
    for path, text in edits.items():
        diff = difflib.unified_diff(
            read(path).splitlines(), text.splitlines(), f"a/{path}", f"b/{path}", lineterm=""
        )
        print("\n".join(diff))


def require_clean_tree() -> None:
    if out("git", "status", "--porcelain", "--untracked-files=no").strip():
        raise ReleaseError("the working tree has uncommitted changes; commit or stash them first")


def fetch_main() -> str:
    run("git", "fetch", "--tags", REMOTE, MAIN)
    return out("git", "rev-parse", f"{REMOTE}/{MAIN}").strip()


def require_no_tag(tag: str) -> None:
    if run("git", "rev-parse", "-q", "--verify", f"refs/tags/{tag}", check=False).returncode == 0:
        raise ReleaseError(f"the tag {tag} already exists locally")
    if out("git", "ls-remote", "--tags", REMOTE, f"refs/tags/{tag}").strip():
        raise ReleaseError(f"the tag {tag} already exists on {REMOTE}")


def remote_main() -> str:
    listing = out("git", "ls-remote", REMOTE, f"refs/heads/{MAIN}")
    return listing.split()[0] if listing.strip() else ""


def require_no_branch(branch: str) -> None:
    local = run("git", "rev-parse", "-q", "--verify", f"refs/heads/{branch}", check=False)
    if local.returncode == 0:
        raise ReleaseError(f"the branch {branch} already exists locally")
    if out("git", "ls-remote", "--heads", REMOTE, f"refs/heads/{branch}").strip():
        raise ReleaseError(f"the branch {branch} already exists on {REMOTE}")


def open_pull_request(
    base: str,
    branch: str,
    edits: dict[str, str],
    title: str,
    body: str,
    assume_yes: bool,
    verify: Callable[[], None] | None = None,
) -> bool:
    if not confirm(f"Create the branch {branch} and write {len(edits)} file(s)?", assume_yes):
        return False
    show("git", "switch", "--no-track", "-c", branch, base)
    state = f"You are on the new local branch {branch}; nothing was committed or pushed."
    undo = [
        "git restore --staged --worktree .",
        f"git switch {MAIN}",
        f"git branch -D {branch}",
    ]

    def report() -> str:
        return "\n".join([state, "To start over:", *(f"  {command}" for command in undo)])

    try:
        write_files(edits)
        if verify:
            verify()
        if not confirm(f"Commit as '{title}'?", assume_yes):
            print(f"Stopped. {report()}")
            return False
        show("git", "add", "--", *edits)
        show("git", "commit", "-m", title)
        state = f"The commit is on the local branch {branch}; nothing was pushed."
        undo = undo[1:]
        if not confirm(f"Push {branch} and open a pull request against {MAIN}?", assume_yes):
            print(f"Stopped. {report()}")
            return False
        undo = [*undo, f"git push {REMOTE} --delete {branch}"]
        state = f"The push of {branch} may not have completed."
        show("git", "push", "-u", REMOTE, branch)
        state = (
            f"The branch {branch} is pushed, without a pull request. Open it with:\n"
            f"  gh pr create -R {_slug[0] if _slug else 'OWNER/REPO'} --base {MAIN} "
            f"--head {branch} --fill"
        )
        show(
            "gh", "pr", "create", "--base", MAIN, "--head", branch, "--title", title, "--body", body
        )
    except (ReleaseError, OSError) as error:
        raise ReleaseError(f"{error}\n{report()}") from error
    except (KeyboardInterrupt, EOFError):
        print(f"\n{report()}", file=sys.stderr)
        raise
    return True


def verify_lockfile() -> None:
    if not shutil.which("cargo"):
        print("cargo is not on the PATH: Cargo.lock is not checked here; CI checks it.")
        return
    run("cargo", "metadata", "--locked", "--format-version", "1")
    print("Cargo.lock agrees with the manifests.")


def cmd_prepare(args: argparse.Namespace) -> None:
    base = fetch_main()
    read = reader_at(base)
    current = current_version(read("Cargo.toml"))
    version = next_version(current, args.version)
    tag, branch = f"v{version}", f"release/{version}"
    require_no_tag(tag)
    date = args.date or datetime.date.today().isoformat()
    try:
        datetime.date.fromisoformat(date)
    except ValueError as error:
        raise ReleaseError(f"'{date}' is not a date of the form YYYY-MM-DD") from error
    edits = release_edits(read, version, date)

    print(f"Release {current} -> {version} ({date}), from {REMOTE}/{MAIN} at {base[:7]}\n")
    notes = release_notes(read("CHANGELOG.md"), "Unreleased")
    print(f"Release notes ('## Unreleased' of CHANGELOG.md):\n\n{notes}\n")
    if args.dry_run:
        print_diff(read, edits)
        return
    require_clean_tree()
    require_no_branch(branch)
    body = (
        f"Release {version}: version in `Cargo.toml`, `Cargo.lock`, `CITATION.cff` and the "
        f"documentation, and the `CHANGELOG.md` section.\n\n"
        f"After the merge: `uv run scripts/release.py tag {version}`."
    )
    if open_pull_request(
        base, branch, edits, f"release: {version}", body, args.yes, verify_lockfile
    ):
        print(
            f"\nNext: merge the pull request when its checks are green, then run\n"
            f"  uv run scripts/release.py tag {version}"
        )


def workflow_runs(workflow: str, sha: str) -> list[dict]:
    fields = "databaseId,status,conclusion,event,headBranch,url"
    listing = out(
        "gh",
        "run",
        "list",
        "--workflow",
        workflow,
        "--commit",
        sha,
        "--limit",
        "50",
        "--json",
        fields,
    )
    return [run_ for run_ in json.loads(listing) if run_["headBranch"] == MAIN]


def succeeded(runs: list[dict]) -> bool:
    return any(run_["conclusion"] == "success" for run_ in runs)


def running(runs: list[dict]) -> dict | None:
    return next((run_ for run_ in runs if run_["status"] != "completed"), None)


def require_ci(sha: str) -> None:
    runs = [run_ for run_ in workflow_runs(CI_WORKFLOW, sha) if run_["event"] == "push"]
    if succeeded(runs):
        print(f"{CI_WORKFLOW}: green on {sha[:7]}")
        return
    active = running(runs)
    if active:
        show("gh", "run", "watch", str(active["databaseId"]), "--exit-status")
        return
    hint = f"; rerun it with: gh run rerun {runs[0]['databaseId']}" if runs else ""
    raise ReleaseError(
        f"{CI_WORKFLOW} has no successful run for the push of {sha[:7]} to {MAIN}{hint}"
    )


def require_dry_run(sha: str) -> None:
    def manual() -> list[dict]:
        return [
            run_
            for run_ in workflow_runs(RELEASE_WORKFLOW, sha)
            if run_["event"] == "workflow_dispatch"
        ]

    runs = manual()
    if succeeded(runs):
        print(f"{RELEASE_WORKFLOW}: manual run green on {sha[:7]}")
        return
    active = running(runs)
    if not active:
        question = f"{RELEASE_WORKFLOW} has no green manual run on {sha[:7]}. Start one now?"
        if not confirm(question, False):
            raise ReleaseError("the release workflow has not been tried on this commit")
        if remote_main() != sha:
            raise ReleaseError(f"{REMOTE}/{MAIN} moved past {sha[:7]}; run the command again")
        known = {run_["databaseId"] for run_ in runs}
        show("gh", "workflow", "run", RELEASE_WORKFLOW, "--ref", MAIN)
        for _ in range(30):
            time.sleep(4)
            active = next((run_ for run_ in manual() if run_["databaseId"] not in known), None)
            if active:
                break
        else:
            raise ReleaseError(
                f"the manual run of {RELEASE_WORKFLOW} did not appear on {sha[:7]}; "
                f"if {MAIN} moved meanwhile, the run is on a newer commit: cancel it and "
                "run the command again"
            )
    show("gh", "run", "watch", str(active["databaseId"]), "--exit-status")


def cmd_tag(args: argparse.Namespace) -> None:
    sha = fetch_main()
    read = reader_at(sha)
    version = current_version(read("Cargo.toml"))
    if args.version and args.version != version:
        raise ReleaseError(
            f"{REMOTE}/{MAIN} is at version {version}, not {args.version}; "
            "is the release pull request merged?"
        )
    parse_version(version)
    tag = f"v{version}"
    try:
        require_no_tag(tag)
    except ReleaseError as error:
        raise ReleaseError(
            f"{error}; {REMOTE}/{MAIN} is at version {version}. Is the release pull request merged?"
        ) from error
    problems = release_problems(read, version)
    if unreleased_entries(read("CHANGELOG.md")):
        problems.append("CHANGELOG.md has entries under '## Unreleased' that the release lacks")
    if current_version(reader_at(f"{sha}~1")("Cargo.toml")) == version:
        problems.append(
            f"{sha[:7]} is not the release commit: its parent has version {version} too "
            "(commits merged after the release, or a release pull request of several commits "
            "that was not squashed)"
        )
    if problems:
        raise ReleaseError(f"{REMOTE}/{MAIN} is not ready for {tag}:\n  " + "\n  ".join(problems))
    print(f"{REMOTE}/{MAIN} at {sha[:7]} is consistent for {version}")
    require_ci(sha)
    require_dry_run(sha)

    print(f"\nPushing {tag} publishes {version} to PyPI and GitHub; it cannot be undone.")
    if input(f"Type {version} to tag {sha[:7]} and push: ").strip() != version:
        print("Stopped. Nothing was tagged.")
        return
    show("git", "tag", "-a", "-m", f"FastWarp {version}", tag, sha)
    try:
        show("git", "push", REMOTE, f"refs/tags/{tag}")
    except (ReleaseError, KeyboardInterrupt) as error:
        listing = run("git", "ls-remote", "--tags", REMOTE, f"refs/tags/{tag}", check=False)
        if listing.returncode != 0:
            state = f"Whether {tag} reached {REMOTE} is unknown; the local tag {tag} is kept."
        elif listing.stdout.strip():
            state = f"The tag {tag} is on {REMOTE}: the release workflow runs."
        else:
            run("git", "tag", "-d", tag, check=False)
            state = f"The tag {tag} is not on {REMOTE}; the local tag is deleted again."
        raise ReleaseError(f"{str(error) or 'interrupted'}\n{state}") from error
    print(
        f"\nThe release workflow runs now; find it with\n"
        f"  gh run list --workflow {RELEASE_WORKFLOW} --limit 1\n"
        f"When the release is published, run\n"
        f"  uv run scripts/release.py finish {version}"
    )


def fetch_pypi(version: str) -> dict:
    url = f"https://pypi.org/pypi/{PYPI_PROJECT}/{version}/json"
    try:
        with urllib.request.urlopen(url, timeout=30) as response:
            return json.load(response)
    except urllib.error.HTTPError as error:
        if error.code == 404:
            raise ReleaseError(f"{PYPI_PROJECT} {version} is not on PyPI") from error
        raise ReleaseError(f"PyPI answered {error.code} for {url}") from error
    except urllib.error.URLError as error:
        raise ReleaseError(f"cannot reach PyPI: {error.reason}") from error


def cmd_finish(args: argparse.Namespace) -> None:
    base = fetch_main()
    read = reader_at(base)
    version = args.version or current_version(read("Cargo.toml"))
    parse_version(version)
    tag, branch = f"v{version}", f"chore/benchmark-pin-{version}"

    release = json.loads(out("gh", "release", "view", tag, "--json", "isDraft,url"))
    if release["isDraft"]:
        raise ReleaseError(
            f"the GitHub release {tag} is still a draft; the workflow has not finished"
        )
    wheels = wheel_hashes(fetch_pypi(version))
    sums = parse_sha256sums(
        out("gh", "release", "download", tag, "--pattern", "SHA256SUMS", "--output", "-")
    )
    mismatches = hash_mismatches(wheels, sums)
    if mismatches:
        raise ReleaseError(
            "the wheels on PyPI differ from the GitHub release:\n  " + "\n  ".join(mismatches)
        )
    print(f"{len(wheels)} wheels of {version} on PyPI match SHA256SUMS of {release['url']}")

    edits = {PIN_FILE: render_pin(version, wheels)}
    if edits[PIN_FILE] == read(PIN_FILE):
        print(f"{PIN_FILE} already pins {version}.")
    elif args.dry_run:
        print_diff(read, edits)
    else:
        require_clean_tree()
        require_no_branch(branch)
        title = f"chore: pin the benchmark to fastwarp {version}"
        body = (
            f"`{PIN_FILE}` pins the wheels of {version}; their SHA-256 on PyPI match "
            f"`SHA256SUMS` of the GitHub release."
        )
        open_pull_request(base, branch, edits, title, body, args.yes)
    print("\nRemaining manual checks: 'After the release' in RELEASING.md.")


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        prog="release.py", description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    commands = parser.add_subparsers(dest="command", required=True)

    prepare = commands.add_parser("prepare", help="open the release pull request")
    prepare.add_argument("version", help="X.Y.Z, or major, minor or patch")
    prepare.add_argument("--date", help="release date, YYYY-MM-DD (default: today)")
    prepare.set_defaults(run=cmd_prepare)

    tag = commands.add_parser("tag", help="check the merged release, then tag and push")
    tag.add_argument("version", nargs="?", help=f"expected version (default: that of {MAIN})")
    tag.set_defaults(run=cmd_tag)

    finish = commands.add_parser("finish", help="pin the benchmark to the published wheels")
    finish.add_argument("version", nargs="?", help=f"released version (default: that of {MAIN})")
    finish.set_defaults(run=cmd_finish)

    for command in (prepare, finish):
        command.add_argument("--dry-run", action="store_true", help="print the changes only")
        command.add_argument("--yes", action="store_true", help="answer yes to every question")

    args = parser.parse_args(argv)
    for stream in (sys.stdout, sys.stderr):
        if hasattr(stream, "reconfigure"):
            stream.reconfigure(errors="replace")
    try:
        args.run(args)
    except (ReleaseError, OSError, ValueError) as error:
        print(f"error: {error}", file=sys.stderr)
        return 1
    except (KeyboardInterrupt, EOFError):
        print("\nStopped.", file=sys.stderr)
        return 130
    return 0


if __name__ == "__main__":
    sys.exit(main())
