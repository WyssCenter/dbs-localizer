@REM SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
@REM SPDX-License-Identifier: GPL-3.0-only

@echo off
set /p options=
echo {"type":"stage_started","stage":"normalize"}
echo {"type":"stage_awaiting_qc","stage":"normalize"}
echo {"type":"stage_failed","stage":"normalize","message":"norm: qc not approved"}
echo {"type":"pipeline_completed","stages_ok":0,"stages_failed":1,"stages_skipped":0}
exit /b 1
