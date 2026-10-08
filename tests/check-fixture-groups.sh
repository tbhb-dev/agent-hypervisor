#!/usr/bin/env bash
set -u

groups="$1"
# A fixture records its process group at spawn, including when its test panics.
# Give killed grandchildren a short interval to leave before declaring a leak.
for attempt in 1 2 3 4 5 6 7 8 9 10; do
  live=()
  malformed=()
  while IFS= read -r pid || [[ -n "$pid" ]]; do
    if [[ ! "$pid" =~ ^[1-9][0-9]*$ ]]; then
      malformed+=("$pid")
    elif kill -0 -- "-$pid" 2>/dev/null; then
      live+=("$pid")
    fi
  done < "$groups"
  if ((${#malformed[@]} != 0)); then
    printf 'malformed fixture process group record: %q\n' "${malformed[@]}" >&2
    exit 1
  fi
  if ((${#live[@]} == 0)); then
    printf 'verified: no fixture process groups survived the test run\n'
    exit 0
  fi
  sleep 0.1
done

printf 'fixture process groups still alive after tests: %s\n' "${live[*]}" >&2
exit 1
