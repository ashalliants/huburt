#!/bin/zsh
# Walks the app through states, measuring memory after each step.
# Usage: tools/memtest.sh <path to .app> <scratch dir> "<HUB_TEST_STEPS>" "<seconds to wait before each measurement, space separated>"
APP=$1; S=$2; STEPS=$3; WAITS=$4; cd "$(dirname "$0")/.."
python3 tools/measure.py snapshot > $S/before.json
open -n --env HUB_TEST_STEPS="$STEPS" --env HUB_IDLE_SECONDS=${HUB_IDLE_SECONDS:-20} --stdout $S/memtest.out --stderr $S/memtest.out "$APP"
sleep 4; P=$(pgrep -x huburt); echo "pid=$P"
for t in ${=WAITS}; do sleep $t; echo "--- +${t}s: $(tail -n 1 ~/Library/Logs/Huburt.log)"; python3 tools/measure.py total $P $S/before.json; done
kill $P
