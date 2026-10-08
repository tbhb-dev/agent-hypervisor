#!/usr/bin/env bash
set -u

jobs="$1"
# Each launchd test records its job before bootstrap. A job still loaded after the
# suite is a leak: boot it out so nothing stays registered, then fail.
leaked=()
while IFS= read -r service || [[ -n "$service" ]]; do
  if [[ ! "$service" =~ ^gui/[0-9]+/dev\.tbhb\.hypervisor\.test\.[A-Za-z0-9._-]+$ ]]; then
    printf 'malformed launchd test job record: %q\n' "$service" >&2
    exit 1
  fi
  if launchctl print "$service" >/dev/null 2>&1; then
    launchctl bootout "$service"
    leaked+=("$service")
  fi
done < "$jobs"
if ((${#leaked[@]} != 0)); then
  printf 'launchd test jobs still loaded after tests: %s\n' "${leaked[*]}" >&2
  exit 1
fi
printf 'verified: no launchd test jobs survived the test run\n'
