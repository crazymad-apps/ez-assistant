#!/bin/bash

cd "$(dirname "$0")" || exit 1
pkill -f 'ez-assistant-runtime serve'
node scripts/run-cargo.mjs run -p assistant-runtime-host -- launch
