#!/usr/bin/env bash
set -uo pipefail
cd "$(dirname -- "${BASH_SOURCE[0]}")/.."

pay_log=$(mktemp)
receive_log=''
pay_pid=''
status=0

coproc RECEIVER { ./lsp-cli create-invoice -u 1 --amount 1 --expiry 3600 --timeout 120 --timings 2>&1; }
receive_pid=$RECEIVER_PID
exec {receive_fd}<&"${RECEIVER[0]}"

while IFS= read -r -u "$receive_fd" line; do
    receive_log+="$line"$'\n'
    if [[ $line == invoice_address：* && -z $pay_pid ]]; then
        invoice=${line#invoice_address：}
        ./lsp-cli pay -u 2 "$invoice" --timeout 120 --timings >"$pay_log" 2>&1 &
        pay_pid=$!
    fi
done

wait "$receive_pid" || status=$?
if [[ -n $pay_pid ]]; then
    wait "$pay_pid" || status=$?
else
    status=1
fi

printf '%s\n%s%s\n' '------' "$receive_log" '------'
cat "$pay_log"
rm "$pay_log"
exit "$status"
