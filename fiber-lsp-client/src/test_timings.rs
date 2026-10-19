use super::*;

#[test]
fn payment_steps_keep_observation_order_and_ignore_replays() {
    enable(true);
    start_payment();
    payment_step("received", "收到付款");
    payment_step("signed", "签名提交");
    payment_step("received", "重放不应覆盖");
    STATE.with_borrow(|state| {
        assert_eq!(state.steps.len(), 2);
        assert_eq!(state.steps[0].id, "received");
        assert_eq!(state.steps[0].label, "收到付款");
        assert_eq!(state.steps[1].id, "signed");
        assert_eq!(
            state.steps[1].elapsed_us,
            state.steps[0].since_previous_us + state.steps[1].since_previous_us
        );
        let json = serde_json::to_value(&state.steps).unwrap();
        assert_eq!(json[1]["id"], "signed");
        assert!(json[1]["since_previous_us"].is_number());
    });
    enable(false);
    start_payment();
    payment_step("disabled", "不应记录");
    STATE.with_borrow(|state| {
        assert!(state.payment_start.is_none());
        assert!(state.steps.is_empty());
    });
}

#[test]
fn payment_report_shows_intervals_totals_and_unfinished_tail() {
    let state = State {
        payment_start: Some(Instant::now()),
        steps: vec![
            PaymentStep {
                id: "invoice_ready",
                label: "发票输出完成",
                elapsed_us: 100_000,
                since_previous_us: 100_000,
            },
            PaymentStep {
                id: "received",
                label: "观察到 Received",
                elapsed_us: 2_100_000,
                since_previous_us: 2_000_000,
            },
        ],
        ..Default::default()
    };
    let report = payment_report(&state, true);
    assert!(report.contains("[1]\t[付款]\t100.0ms\t100.0ms\t发票输出完成"));
    assert!(report.contains("[2]\t[付款]\t2000.0ms\t2100.0ms\t观察到 Received"));
    assert!(!report.contains("未完成"));
    assert!(payment_report(&state, false).contains("未完成，停止推进"));
    assert!(payment_report(&State::default(), false).is_empty());
}

#[test]
fn receipt_latency_keeps_protocol_wait_and_does_not_subtract_all_event_waits() {
    let mut metrics = BTreeMap::new();
    for (name, total_us) in [
        ("cli.total", 11_000_000),
        ("payment.wait_for_receipt", 10_000_000),
        ("payment.receipt_processing", 1_000_000),
        ("payment.receipt_protocol_wait", 400_000),
        ("payment.event_wait", 10_400_000),
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
