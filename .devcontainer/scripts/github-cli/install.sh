#!/usr/bin/env bash
set -euo pipefail

if command -v gh >/dev/null 2>&1; then
  exit 0
fi

if ! command -v apt-get >/dev/null 2>&1; then
  echo "Unsupported package manager; install GitHub CLI manually." >&2
  exit 1
fi

SUDO=""
if [ "$(id -u)" -ne 0 ]; then
  SUDO="sudo"
fi

${SUDO} apt-get update
${SUDO} apt-get install -y curl ca-certificates gnupg
${SUDO} mkdir -p /etc/apt/keyrings
curl -fsSL https://cli.github.com/packages/githubcli-archive-keyring.gpg | ${SUDO} tee /etc/apt/keyrings/githubcli-archive-keyring.gpg >/dev/null
${SUDO} chmod go+r /etc/apt/keyrings/githubcli-archive-keyring.gpg
echo "deb [arch=$(dpkg --print-architecture) signed-by=/etc/apt/keyrings/githubcli-archive-keyring.gpg] https://cli.github.com/packages stable main" | ${SUDO} tee /etc/apt/sources.list.d/github-cli.list >/dev/null
${SUDO} apt-get update
${SUDO} apt-get install -y gh
