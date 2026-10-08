#!/usr/bin/env bash
set -u

groups="$(mktemp)"
jobs="$(mktemp)"
trap 'rm -f "$groups" "$jobs"' EXIT
export HYPERVISOR_TEST_GROUPS="$groups"
export HYPERVISOR_TEST_LAUNCHD="$jobs"

if cargo test --workspace --locked; then
  test_status=0
else
  test_status=$?
fi

if [ "$test_status" -eq 0 ]; then
  for ((attempt = 1; attempt <= 50; attempt++)); do
    printf 'byte replay repeat %d/50\n' "$attempt"
    if ! cargo test --quiet --locked -p hypervisord --test conformance terminal_channel_replays_retained_bytes_and_snapshots_evicted_bytes -- --exact; then
      test_status=1
      break
    fi
  done
fi

bash tests/check-fixture-groups.sh "$groups" || exit 1
bash tests/check-launchd-jobs.sh "$jobs" || exit 1
exit "$test_status"
