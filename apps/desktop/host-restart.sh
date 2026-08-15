#! /bin/bash
pkill -f 'ez-assistant-runtime serve'
cargo run -p assistant-runtime-host -- launch
