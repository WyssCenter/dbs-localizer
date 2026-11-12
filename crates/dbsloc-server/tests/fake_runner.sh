#!/bin/sh
# SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
# SPDX-License-Identifier: GPL-3.0-only

# Fake dbsloc_pipeline_runner for integration tests (Unix): two RunEvent NDJSON lines, exit 0.
printf '%s\n' '{"type":"stage_started","stage":"bids"}'
printf '%s\n' '{"type":"pipeline_completed","stages_ok":1,"stages_failed":0,"stages_skipped":0}'
exit 0
