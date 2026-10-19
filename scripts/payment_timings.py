"""Merge the two local CLI timing logs onto one payment timeline."""

import json
import sys
from pathlib import Path


def duration(us):
    return f"{us / 1_000:.1f}ms"


def parse_log(log):
    report = None
    messages = []
    for line in log.splitlines():
        if line.startswith("timings "):
            report = json.loads(line.removeprefix("timings "))
            messages.extend(report.get("human_report", "").splitlines())
        elif line:
            messages.append(line)
    return report, messages


def merged_report(receive_log, pay_log):
    participants = [
        ("收款方 A", *parse_log(receive_log)),
        ("付款方 B", *parse_log(pay_log)),
    ]
    out = []
    events = []
    starts = []
    complete = True
    for role, report, messages in participants:
        out.append(f"\n------ {role} ------")
        # Use the CLI's own rendering so each side retains exactly the same
        # timing details as --timings, followed by the merged timeline below.
        out.extend(
            message if message.startswith("[") and "\t" in message else f"[{role}] {message}"
            for message in messages
        )
        if report is None or report.get("payment_started_unix_us") is None:
            out.append(f"[{role}] 缺少共同时间基准，无法合并该端时序；请重新编译客户端")
            complete = False
            continue
        start = report["payment_started_unix_us"]
        starts.append(start)
        for step in report["payment_steps"]:
            events.append((start + step["elapsed_us"], role, step["label"]))
        if not report["success"]:
            complete = False
            events.append(
                (start + report["payment_elapsed_us"], role, "未完成，停止推进")
            )

    out.append("\n收付款完整时序（本次时间 / 从流程开始总共耗时）")
    if not starts:
        out.append("没有可合并的步骤")
        return "\n".join(out) + "\n", False
    origin = min(starts)
    previous = origin
    for index, (observed, role, label) in enumerate(sorted(events), 1):
        out.append(
            f"[{index}]\t[{role}]\t{duration(observed - previous)}\t"
            f"{duration(observed - origin)}\t{label}"
        )
        previous = observed
    out.append(f"结果：{'双方完成' if complete else '未全部完成'}")
    out.append("按同机两端的本地观察时间合并；区间包含 RPC、签名和协议等待。")
    return "\n".join(out) + "\n", complete


def main():
    output, success = merged_report(sys.stdin.read(), Path(sys.argv[1]).read_text())
    print(output, end="")
    return 0 if success else 1


if __name__ == "__main__":
    sys.exit(main())
