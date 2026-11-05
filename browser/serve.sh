#!/bin/sh
# Serves a fresh copy of the fixture vault for the browser suite.
set -eu
cd "$(dirname "$0")"
rm -rf .tmp
mkdir -p .tmp
cp -R ../tests/fixtures/vault .tmp/vault
exec ../target/release/tachylite serve .tmp/vault .tmp/out --port 7431
