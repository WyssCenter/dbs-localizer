# SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
# SPDX-License-Identifier: GPL-3.0-only

import re
from pathlib import Path

from sphinx.util import logging

_SRC = Path(__file__).resolve().parent
_ROOT = _SRC.parent
_REPO = "https://github.com/WyssCenter/dbs-localizer"
_BRANCH = "main"

_cargo = (_ROOT / "Cargo.toml").read_text(encoding="utf-8")
_version = re.search(r'^version\s*=\s*"([^"]+)"', _cargo, re.MULTILINE)

project = "Wyss DBS Localizer"
author = "Wyss Center for Bio and Neuroengineering"
copyright = "2026 Wyss Center for Bio and Neuroengineering"
release = _version.group(1) if _version else ""
version = release

extensions = [
    "myst_parser",
    "sphinx_copybutton",
    "sphinx_design",
]

exclude_patterns = [
    "_build",
    "requirements.txt",
    "USER_GUIDE.md",
    "PHASE_HISTORY.md",
    "PHASE_C_ROADMAP.md",
    "STATUS.md",
    "SERVER_CLIENT_PLAN.md",
    "BRAND.md",
    "ATLAS_AHEAD.md",
    "ATLAS_DISTAL.md",
    "gpu_*_report.md",
    "agent-rules",
]
myst_heading_anchors = 4
myst_enable_extensions = ["colon_fence", "deflist"]

linkcheck_anchors_ignore_for_url = [r"https://github\.com/.*"]
linkcheck_ignore = [r"https://doi\.org/10\.(1002|1159|3390)/.*"]

copybutton_exclude = ".linenos, .gp, .go"

html_theme = "pydata_sphinx_theme"
html_title = f"Wyss DBS Localizer {release}"
html_favicon = "../crates/dbsloc-viz/assets/brand/dbs-loc-icon-256.png"
html_static_path = [
    "_static",
    "../crates/dbsloc-viz/assets/fonts",
    "../crates/dbsloc-viz/assets/brand",
]
html_css_files = ["dbsloc.css"]
html_show_sourcelink = False
html_context = {
    "github_user": "WyssCenter",
    "github_repo": "dbs-localizer",
    "github_version": _BRANCH,
    "doc_path": "docs",
    "default_mode": "auto",
}
html_theme_options = {
    "logo": {
        "image_light": "_static/wyss-center-icon-256.png",
        "image_dark": "_static/wyss-center-icon-inverse-256.png",
        "text": "Wyss DBS Localizer",
        "alt_text": "Wyss DBS Localizer - Home",
    },
    "navbar_align": "left",
    "header_links_before_dropdown": 5,
    "navigation_with_keys": False,
    "show_toc_level": 2,
    "use_edit_page_button": True,
    "secondary_sidebar_items": {
        "**": ["page-toc", "edit-this-page"],
        "changelog": ["page-toc"],
        "contributing": ["page-toc"],
        "code-of-conduct": ["page-toc"],
        "installation": ["page-toc"],
        "quickstart": ["page-toc"],
        "support": ["page-toc"],
        "citing": ["page-toc"],
        "licence": ["page-toc"],
    },
    "footer_start": ["copyright"],
    "footer_end": ["sphinx-version", "theme-version"],
    "icon_links": [
        {
            "name": "GitHub",
            "url": _REPO,
            "icon": "fa-brands fa-github",
        },
    ],
}
html_sidebars = {
    "index": [],
}

_ROOT_PAGES = {
    "CHANGELOG.md": "changelog",
    "CONTRIBUTING.md": "contributing",
    "CODE_OF_CONDUCT.md": "code-of-conduct",
}
_README_ANCHORS = {"#accuracy": "/guide/lead-tutor.md#accuracy"}
_LINK = re.compile(r"(!?\[[^\]]*\]\()([^)\s]+)(\))")
_FENCE = re.compile(r"^\s*(`{3,}|~{3,})")
_SCHEME = re.compile(r"^[A-Za-z][A-Za-z0-9+.-]*:")
_logger = logging.getLogger(__name__)


def _target(raw, base, docs, where):
    if raw.startswith(("#", "/")) or _SCHEME.match(raw):
        return raw
    path, _, fragment = raw.partition("#")
    fragment = f"#{fragment}" if fragment else ""
    resolved = (base / path).resolve()
    try:
        in_repo = resolved.relative_to(_ROOT).as_posix()
    except ValueError:
        return raw
    if not resolved.exists():
        _logger.warning("link target %s does not exist", raw, location=where)
        return raw
    if in_repo in _ROOT_PAGES:
        return f"/{_ROOT_PAGES[in_repo]}.md{fragment}"
    try:
        in_docs = resolved.relative_to(_SRC)
    except ValueError:
        in_docs = None
    if (
        in_docs is not None
        and resolved.suffix == ".md"
        and in_docs.with_suffix("").as_posix() in docs
    ):
        return f"/{in_docs.as_posix()}{fragment}"
    kind = "tree" if resolved.is_dir() else "blob"
    return f"{_REPO}/{kind}/{_BRANCH}/{in_repo}{fragment}"


def _replace(match, base, docs, where):
    if match[1].startswith("!"):
        return match[0]
    return match[1] + _target(match[2], base, docs, where) + match[3]


def _rewrite_links(text, base, docs, where):
    out = []
    fence = ""
    for line in text.splitlines(keepends=True):
        marker = _FENCE.match(line)
        if marker and not fence:
            fence = marker[1]
        elif marker and marker[1][0] == fence[0] and len(marker[1]) >= len(fence):
            fence = ""
        elif not fence:
            line = _LINK.sub(lambda m: _replace(m, base, docs, where), line)
        out.append(line)
    return "".join(out)


def _on_source_read(app, docname, source):
    base = (_SRC / docname).parent
    source[0] = _rewrite_links(source[0], base, app.env.found_docs, docname)


def _on_include_read(app, relative_path, parent_docname, content):
    included = (_SRC / relative_path).resolve()
    text = content[0]
    if included == _ROOT / "README.md":
        for anchor, page in _README_ANCHORS.items():
            text = text.replace(f"]({anchor})", f"]({page})")
    content[0] = _rewrite_links(
        text, included.parent, app.env.found_docs, str(included)
    )


def setup(app):
    app.connect("source-read", _on_source_read)
    app.connect("include-read", _on_include_read)
