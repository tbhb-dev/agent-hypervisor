#!/usr/bin/env bash
set -u

groups="$(mktemp)"
trap 'rm -f "$groups"' EXIT
export HYPERVISOR_TEST_GROUPS="$groups"

if cargo test --workspace --locked; then
  test_status=0
else
  test_status=$?
fi

# A fixture records its process group at spawn, including when its test panics.
# Give killed grandchildren a short interval to leave before declaring a leak.
for attempt in 1 2 3 4 5 6 7 8 9 10; do
  live=()
  while IFS= read -r pid; do
    if kill -0 -- "-$pid" 2>/dev/null; then
      live+=("$pid")
    fi
  done < "$groups"
  if ((${#live[@]} == 0)); then
    break
  fi
  sleep 0.1
done

if ((${#live[@]} != 0)); then
  printf 'fixture process groups still alive after tests: %s\n' "${live[*]}" >&2
  exit 1
fi
printf 'verified: no fixture process groups survived the test run\n'
exit "$test_status"
