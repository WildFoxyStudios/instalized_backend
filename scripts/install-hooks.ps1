#!/usr/bin/env pwsh
# Install the pre-commit hook for this repo. Safe to re-run.
$ErrorActionPreference = "Stop"
git config core.hooksPath .githooks
Write-Host "pre-commit hook installed (core.hooksPath = .githooks)" -ForegroundColor Green
