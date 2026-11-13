#!/bin/sh
# SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
# SPDX-License-Identifier: GPL-3.0-only

read -r _options
printf '%s\n' '{"type":"stage_started","stage":"normalize"}'
printf '%s\n' '{"type":"stage_awaiting_qc","stage":"normalize"}'
printf '%s\n' '{"type":"stage_failed","stage":"normalize","message":"norm: qc not approved"}'
printf '%s\n' '{"type":"pipeline_completed","stages_ok":0,"stages_failed":1,"stages_skipped":0}'
exit 1
