#!/usr/bin/env bash
set -uo pipefail
cd "$(dirname -- "${BASH_SOURCE[0]}")/.."

source scripts/config.sh

pay_log=$(mktemp)
receive_log=''
pay_pid=''
status=0

coproc RECEIVER { exec ./lsp-cli create-invoice --rpc-url "$RPC_URL" --ws-url "$WS_URL" -u 1 --amount 1 --expiry 3600 --timeout 120 --timings-json 2>&1; }
receive_pid=$RECEIVER_PID
exec {receive_fd}<&"${RECEIVER[0]}"

while IFS= read -r -u "$receive_fd" line; do
    receive_log+="$line"$'\n'
    if [[ $line == invoice_address：* && -z $pay_pid ]]; then
        invoice=${line#invoice_address：}
        # Keep the in-memory preimage; always resume if the script exits here.
        trap 'kill -CONT "$receive_pid" 2>/dev/null || true' EXIT
        kill -STOP "$receive_pid" || exit "$?"
        printf '[收款方 A] 发票已创建，暂停收款进程以模拟离线\n'

        exec {pay_fd}< <(exec ./lsp-cli pay --rpc-url "$RPC_URL" --ws-url "$WS_URL" -u 2 "$invoice" --timeout 120 --timings-json 2>&1)
        pay_pid=$!
        sed -u '/^付款已提交，等待结算$/q' <&"$pay_fd" >"$pay_log"
        sleep "${OFFLINE_SECONDS:-5}"
        kill -CONT "$receive_pid" || status=$?
        trap - EXIT
        printf '[收款方 A] 已恢复收款进程，继续签名及结算\n'
        cat <&"$pay_fd" >>"$pay_log" &
        pay_reader_pid=$!
        exec {pay_fd}<&-
    fi
done

wait "$receive_pid" || status=$?
if [[ -n $pay_pid ]]; then
    wait "$pay_pid" || status=$?
    wait "$pay_reader_pid" || status=$?
else
    status=1
fi

printf '%s' "$receive_log" | python3 scripts/payment_timings.py "$pay_log" || status=$?
rm "$pay_log"
exit "$status"
