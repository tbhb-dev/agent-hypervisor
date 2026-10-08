#!/usr/bin/env bash
set -u

groups="$(mktemp)"
jobs="$(mktemp)"
trap 'rm -f "$groups" "$jobs"' EXIT
export HYPERVISOR_TEST_GROUPS="$groups"
export HYPERVISOR_TEST_LAUNCHD="$jobs"

mode="${1:-check}"
if [ "$mode" = host ]; then
  if [ "$(uname -s)" != Darwin ]; then
    printf 'check:host requires an unsandboxed macOS host\n' >&2
    exit 2
  fi
  printf 'check:host: running every launchd and seatbelt integration test on this host\n'
  test_command=(cargo test --locked -p hypervisord --test launchd --test seatbelt_driver -- --include-ignored)
elif [ "$mode" = check ]; then
  test_command=(cargo test --workspace --locked)
else
  printf 'unknown test mode: %s\n' "$mode" >&2
  exit 2
fi

if "${test_command[@]}"; then
  test_status=0
else
  test_status=$?
fi

if [ "$mode" = check ] && [ "$test_status" -eq 0 ]; then
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
if [ "$mode" = check ]; then
  printf 'HOST TESTS NOT RUN BY check: launchd and seatbelt integration require mise run check:host on an unsandboxed macOS host before merge\n' >&2
elif [ "$test_status" -ne 0 ]; then
  printf 'check:host failed: run on an unsandboxed macOS host and record the result for this head SHA\n' >&2
fi
exit "$test_status"
