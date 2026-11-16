# SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
# SPDX-License-Identifier: Apache-2.0

"""The ``fastwarp`` command line: ``python -m fastwarp`` and the ``fastwarp`` console script.

Both run the command line built into the extension, the same code as the standalone
``fastwarp`` program, so options, output and exit status are the same.
"""

from __future__ import annotations

import signal
import sys
import threading
from collections.abc import Sequence

from ._core import cli_main


def main(argv: Sequence[str] | None = None) -> int:
    """Run the command line on ``argv`` (default: ``sys.argv[1:]``) and return its exit status.

    Ctrl-C stops the program at once, as it stops the standalone program; a file being written
    at that moment may be left incomplete.
    """
    args = list(sys.argv[1:] if argv is None else argv)
    sys.stdout.flush()
    sys.stderr.flush()
    if threading.current_thread() is not threading.main_thread():
        return cli_main(args)
    previous = signal.signal(signal.SIGINT, signal.SIG_DFL)
    try:
        return cli_main(args)
    finally:
        signal.signal(signal.SIGINT, previous)


if __name__ == "__main__":
    sys.exit(main())
