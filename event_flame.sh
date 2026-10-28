#!/bin/bash -x
# Although the target dir is named pool-stats, QJS_POOL_STATS=1 is not added.
CARGO_TARGET_DIR=target/pool-stats cargo build -p servoshell --features tracing --config 'patch.crates-io.mozjs.path="../servo-quickjs/mozjs"'
rm -rf flames/*
SERVO_EVENT_FLAME_DIR=./flames SERVO_EVENT_FLAME_CLASS=HTMLDivElement SERVO_TRACING=mozjs_gc=trace ./mach run --profile pool-stats/debug ./tests/gc/2-remove-subtree.html
python event_flame_to_html.py flames
