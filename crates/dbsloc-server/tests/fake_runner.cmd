@REM SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
@REM SPDX-License-Identifier: GPL-3.0-only

@echo off
rem Fake dbsloc_pipeline_runner for integration tests (Windows): emit two RunEvent NDJSON lines.
echo {"type":"stage_started","stage":"bids"}
echo {"type":"pipeline_completed","stages_ok":1,"stages_failed":0,"stages_skipped":0}
exit /b 0
