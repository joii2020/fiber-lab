import json
import unittest

from payment_timings import merged_report


def log(start, steps, success=True, elapsed=10_000, human_report=""):
    return "timings " + json.dumps(
        {
            "payment_started_unix_us": start,
            "payment_steps": [
                {"id": label, "label": label, "elapsed_us": elapsed_us}
                for elapsed_us, label in steps
            ],
            "payment_elapsed_us": elapsed,
            "success": success,
            "human_report": human_report,
        }
    )


class PaymentTimingsTests(unittest.TestCase):
    def test_individual_cli_reports_precede_the_combined_timeline(self):
        output, success = merged_report(
            log(
                1_000_000,
                [(100, "发票输出")],
                human_report="收付款时序\n[1]\t[收款]\t0.1ms\t0.1ms\tA 本端耗时\n耗时统计\n签名累计：10 毫秒",
            ) + "\nA 后续消息",
            log(
                1_002_000,
                [(100, "付款提交")],
                human_report="收付款时序\nB 本端耗时\n耗时统计\nRPC 累计：20 毫秒",
            ),
        )
        self.assertTrue(success)
        self.assertLess(output.index("A 本端耗时"), output.index("A 后续消息"))
        self.assertLess(output.index("A 后续消息"), output.index("B 本端耗时"))
        self.assertLess(output.index("B 本端耗时"), output.index("收付款完整时序"))
        self.assertEqual(output.count("耗时统计"), 2)
        self.assertIn("[收款方 A] 签名累计：10 毫秒", output)
        self.assertIn("[付款方 B] RPC 累计：20 毫秒", output)
        self.assertEqual(output.count("收付款完整时序"), 1)
        row = next(line for line in output.splitlines() if line.endswith("A 本端耗时"))
        self.assertEqual(row.split("\t"), ["[1]", "[收款]", "0.1ms", "0.1ms", "A 本端耗时"])

    def test_merges_by_common_time_instead_of_each_process_elapsed_time(self):
        output, success = merged_report(
            "invoice_address：example\n"
            + log(1_000_000, [(100, "发票输出"), (4_000, "Received")]),
            log(1_002_000, [(1_000, "付款提交"), (3_000, "付款完成")]),
        )
        self.assertTrue(success)
        self.assertIn("[收款方 A] invoice_address：example", output)
        self.assertIn("[2]\t[付款方 B]\t2.9ms\t3.0ms\t付款提交", output)
        self.assertIn("[3]\t[收款方 A]\t1.0ms\t4.0ms\tReceived", output)
        self.assertIn("[4]\t[付款方 B]\t1.0ms\t5.0ms\t付款完成", output)
        self.assertEqual(output.count("收付款完整时序"), 1)
        self.assertNotIn("timings {", output)

    def test_failure_keeps_both_progress_and_original_error(self):
        output, success = merged_report(
            log(1_000, [(100, "发票输出")], False, 5_000) + "\n错误：超时",
            log(2_000, [(100, "付款提交")]),
        )
        self.assertFalse(success)
        self.assertIn("[收款方 A] 错误：超时", output)
        self.assertIn("[3]\t[收款方 A]\t3.9ms\t5.0ms\t未完成，停止推进", output)
        self.assertIn("结果：未全部完成", output)

    def test_missing_or_old_report_is_not_reported_as_success(self):
        for receive in ["错误：账户不存在", 'timings {"success": true}']:
            with self.subTest(receive=receive):
                output, success = merged_report(receive, "")
                self.assertFalse(success)
                self.assertIn("缺少共同时间基准", output)
                self.assertIn("没有可合并的步骤", output)


if __name__ == "__main__":
    unittest.main()
