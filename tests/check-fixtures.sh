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

bash tests/check-fixture-groups.sh "$groups" || exit 1
exit "$test_status"
