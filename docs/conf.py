# SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
# SPDX-License-Identifier: Apache-2.0

project = "FastWarp"
author = "Richard Köhler and Jan Philipp Payonk"
copyright = "2026 Wyss Center for Bio and Neuroengineering"

extensions = [
    "myst_parser",
    "autoapi.extension",
]

exclude_patterns = ["_build", "requirements.txt"]
myst_heading_anchors = 3
myst_enable_extensions = ["colon_fence"]

autoapi_dirs = ["../python/fastwarp"]
autoapi_root = "api"
autoapi_file_patterns = ["*.pyi", "*.py"]
autoapi_options = [
    "members",
    "undoc-members",
    "show-module-summary",
    "imported-members",
    "special-members",
]
autoapi_python_class_content = "both"
autoapi_keep_files = False

html_theme = "furo"
html_title = "FastWarp"
