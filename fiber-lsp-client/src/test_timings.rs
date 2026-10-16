use super::*;

#[test]
fn receipt_latency_keeps_protocol_wait_and_does_not_subtract_all_poll_waits() {
    let mut metrics = BTreeMap::new();
    for (name, total_us) in [
        ("cli.total", 11_000_000),
        ("payment.wait_for_receipt", 10_000_000),
        ("payment.receipt_processing", 1_000_000),
        ("payment.receipt_protocol_wait", 400_000),
        ("payment.poll_wait", 10_400_000),
    ] {
        metrics.insert(
            name.into(),
            Metric {
                calls: 1,
                total_us,
                ..Default::default()
            },
        );
    }
    let report = human_report(&metrics, true);
    assert!(report.contains("收款处理耗时：1.000 秒"));
    assert!(report.contains("其中协议等待：400.0 毫秒"));
    assert!(report.contains("等待付款（不计入收款处理）：10.000 秒"));
    assert!(report.contains("命令运行时间（含等待付款）：11.000 秒"));
    metrics
        .get_mut("payment.receipt_processing")
        .unwrap()
        .errors = 1;
    assert!(human_report(&metrics, true).contains("收款处理耗时（尚未完成）"));
    metrics.remove("payment.receipt_processing");
    assert!(human_report(&metrics, true).contains("尚未观察到入账 TLC"));
}
#[test]
fn disabled_and_rpc_outcomes() {
    enable(false);
    drop(Span::new("disabled"));
    STATE.with_borrow(|s| assert!(s.metrics.is_empty()));
    enable(true);
    let mut success = Span::rpc("lsp", "success");
    success.finish(true);
    drop(success);
    let mut failed = Span::rpc("ckb", "failed");
    failed.finish(false);
    drop(failed);
    drop(Span::rpc("lsp", "cancelled"));
    STATE.with_borrow(|s| {
        assert_eq!(s.metrics["rpc.lsp.success"].calls, 1);
        assert_eq!(s.metrics["rpc.ckb.failed"].errors, 1);
        assert_eq!(s.metrics["rpc.lsp.cancelled"].cancelled, 1);
    });
    enable(false);
}
