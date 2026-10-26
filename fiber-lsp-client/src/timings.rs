//! Opt-in, payload-free timings for the current-thread CLI runtime.
use std::{
    cell::RefCell,
    collections::BTreeMap,
    fmt::Write,
    time::{Instant, SystemTime, UNIX_EPOCH},
};

use serde::Serialize;

#[derive(Default, Serialize)]
struct Metric {
    calls: u64,
    errors: u64,
    cancelled: u64,
    total_us: u128,
    max_us: u128,
}

#[derive(Default)]
struct State {
    enabled: bool,
    metrics: BTreeMap<String, Metric>,
    payment_start: Option<Instant>,
    payment_started_unix_us: Option<u128>,
    steps: Vec<PaymentStep>,
}

#[derive(Serialize)]
struct PaymentStep {
    id: &'static str,
    label: &'static str,
    elapsed_us: u128,
    since_previous_us: u128,
}
thread_local! { static STATE: RefCell<State> = RefCell::new(State::default()); }

pub fn enable(enabled: bool) {
    STATE.with_borrow_mut(|state| {
        *state = State {
            enabled,
            ..State::default()
        }
    });
}

pub fn start_payment() {
    STATE.with_borrow_mut(|state| {
        if state.enabled {
            state.payment_start = Some(Instant::now());
            state.payment_started_unix_us = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .ok()
                .map(|duration| duration.as_micros());
            state.steps.clear();
        }
    });
}

/// Record the first local observation only; polling and subscription replay must not
/// move a milestone or turn it into another timing interval.
pub fn payment_step(id: &'static str, label: &'static str) {
    STATE.with_borrow_mut(|state| {
        let Some(start) = state.payment_start else {
            return;
        };
        if state.steps.iter().any(|step| step.id == id) {
            return;
        }
        let elapsed_us = start.elapsed().as_micros();
        let previous = state.steps.last().map_or(0, |step| step.elapsed_us);
        state.steps.push(PaymentStep {
            id,
            label,
            elapsed_us,
            since_previous_us: elapsed_us.saturating_sub(previous),
        });
    });
}

fn payment_report(state: &State, success: bool) -> String {
    let Some(start) = state.payment_start else {
        return String::new();
    };
    let mut out = String::from("\n收付款时序（本端观察，本次时间 / 总共耗时）\n");
    let role = if state.metrics.contains_key("payment.invoice") {
        "收款"
    } else {
        "付款"
    };
    for (i, step) in state.steps.iter().enumerate() {
        writeln!(
            &mut out,
            "[{}]\t[{role}]\t{:.1}ms\t{:.1}ms\t{}",
            i + 1,
            step.since_previous_us as f64 / 1_000.0,
            step.elapsed_us as f64 / 1_000.0,
            step.label,
        )
        .unwrap();
    }
    if !success {
        let previous = state.steps.last().map_or(0, |step| step.elapsed_us);
        let elapsed = start.elapsed().as_micros();
        writeln!(
            &mut out,
            "[{}]\t[{role}]\t{:.1}ms\t{:.1}ms\t未完成，停止推进",
            state.steps.len() + 1,
            elapsed.saturating_sub(previous) as f64 / 1_000.0,
            elapsed as f64 / 1_000.0,
        )
        .unwrap();
    }
    out.push_str("区间包含 RPC、签名和协议等待；收款端等待付款也包含人工复制发票时间。\n");
    out
}

pub struct Span {
    start: Option<Instant>,
    name: String,
    outcome: &'static str,
}
impl Span {
    pub fn new(name: &str) -> Self {
        let enabled = STATE.with_borrow(|state| state.enabled);
        Self {
            start: enabled.then(Instant::now),
            name: if enabled {
                name.to_owned()
            } else {
                String::new()
            },
            outcome: "completed",
        }
    }
    pub fn rpc(service: &str, method: &str) -> Self {
        let mut span = Self::new("");
        if span.start.is_some() {
            span.name = format!("rpc.{service}.{method}");
        }
        span.outcome = "cancelled";
        span
    }
    pub fn finish(&mut self, success: bool) {
        self.outcome = if success { "completed" } else { "error" };
    }
}
impl Drop for Span {
    fn drop(&mut self) {
        let Some(start) = self.start else { return };
        let elapsed = start.elapsed().as_micros();
        STATE.with_borrow_mut(|state| {
            let metric = state
                .metrics
                .entry(std::mem::take(&mut self.name))
                .or_default();
            metric.calls += 1;
            metric.errors += u64::from(self.outcome == "error");
            metric.cancelled += u64::from(self.outcome == "cancelled");
            metric.total_us += elapsed;
            metric.max_us = metric.max_us.max(elapsed);
        });
    }
}

pub fn report(success: bool, json: bool) {
    STATE.with_borrow(|state| {
        if !state.enabled {
            return;
        }
        if json {
            eprintln!(
                "timings {}",
                serde_json::json!({
                    "schema": 1, "unit": "us", "success": success,
                    "profile": if cfg!(debug_assertions) { "debug" } else { "release" },
                    "metrics": state.metrics,
                    "payment_steps": state.steps,
                    "payment_elapsed_us": state.payment_start.map(|start| start.elapsed().as_micros()),
                    "payment_started_unix_us": state.payment_started_unix_us,
                    "human_report": format!("{}{}", payment_report(state, success), human_report(&state.metrics, success)),
                })
            );
        } else {
            eprint!("{}", payment_report(state, success));
            eprint!("{}", human_report(&state.metrics, success));
        }
    });
}

fn duration(us: u128) -> String {
    if us >= 1_000_000 {
        format!("{:.3} 秒", us as f64 / 1_000_000.0)
    } else if us >= 1_000 {
        format!("{:.1} 毫秒", us as f64 / 1_000.0)
    } else {
        format!("{us} 微秒")
    }
}

fn line(out: &mut String, label: &str, metric: &Metric) {
    write!(out, "{label}：{}", duration(metric.total_us)).unwrap();
    if metric.calls > 1 {
        write!(
            out,
            "（{} 次，最长 {}）",
            metric.calls,
            duration(metric.max_us)
        )
        .unwrap();
    }
    if metric.errors > 0 {
        write!(out, "，错误返回 {} 次", metric.errors).unwrap();
    }
    if metric.cancelled > 0 {
        write!(out, "，中断 {} 次", metric.cancelled).unwrap();
    }
    out.push('\n');
}

fn human_report(metrics: &BTreeMap<String, Metric>, success: bool) -> String {
    let mut out = format!(
        "\n耗时统计（{}，{}）\n",
        if cfg!(debug_assertions) {
            "debug"
        } else {
            "release"
        },
        if success { "成功" } else { "未成功" }
    );
    let show = |out: &mut String, name: &str, label: &str| {
        if let Some(metric) = metrics.get(name) {
            line(out, label, metric);
        }
    };
    let receipt = metrics.contains_key("payment.wait_for_receipt");
    if receipt {
        if let Some(processing) = metrics.get("payment.receipt_processing") {
            writeln!(
                &mut out,
                "收款处理耗时{}：{}",
                if processing.errors > 0 {
                    "（尚未完成）"
                } else {
                    ""
                },
                duration(processing.total_us)
            )
            .unwrap();
            show(&mut out, "payment.receipt_protocol_wait", "  其中协议等待");
        } else {
            out.push_str("收款处理耗时：尚未观察到入账 TLC\n");
        }
        show(
            &mut out,
            "payment.wait_for_receipt",
            "等待付款（不计入收款处理）",
        );
    }
    show(
        &mut out,
        "cli.total",
        if receipt {
            "命令运行时间（含等待付款）"
        } else {
            "命令运行时间"
        },
    );
    if let Some(drive) = metrics.get("payment.drive") {
        let operation = metrics
            .get("payment.pay")
            .or_else(|| metrics.get("payment.invoice"));
        if let Some(operation) = operation {
            let preparation = Metric {
                calls: 1,
                total_us: operation.total_us.saturating_sub(drive.total_us),
                ..Default::default()
            };
            line(&mut out, "准备阶段（含账户恢复）", &preparation);
            show(&mut out, "cli.account_restore", "  账户恢复");
            show(&mut out, "rpc.lsp.new_invoice", "  创建发票请求");
        } else {
            show(&mut out, "cli.account_restore", "账户恢复");
        }
        line(
            &mut out,
            if receipt {
                "在线收款推进（含等待付款）"
            } else {
                "支付推进"
            },
            drive,
        );
        show(&mut out, "payment.send", "  提交付款");
        show(
            &mut out,
            "payment.event_wait",
            if receipt {
                "  事件或状态查询等待（含等待付款）"
            } else {
                "  事件或状态查询等待"
            },
        );
        show(&mut out, "payment.external_sign", "  外部签名流程");
        show(
            &mut out,
            "payment.sdk_prepare_confirm",
            "    SDK 校验与签名",
        );
        show(
            &mut out,
            "rpc.lsp.submit_channel_signature",
            "    提交签名请求",
        );
        show(&mut out, "payment.refresh", "  刷新收付款状态");
        if metrics.contains_key("rpc.lsp.settle_invoice")
            || metrics.contains_key("payment.chain_check")
        {
            show(&mut out, "payment.release", "  核验并结算收款");
            show(&mut out, "payment.chain_check", "    独立链上核验");
        }
        show(&mut out, "payment.reconcile", "  完成状态与余额核对");
        if receipt {
            out.push_str("收款处理从本次首次观察到入账 TLC 起计时，包含后续协议等待。\n");
        }
        out.push_str("阶段存在包含关系，请勿逐项相加。\n");
    } else {
        show(&mut out, "cli.account_restore", "账户恢复");
        show(&mut out, "rpc.lsp.new_invoice", "创建发票请求");
    }
    let mut writes = Metric::default();
    for (name, metric) in metrics {
        if name.starts_with("store.") && name.ends_with(".write") {
            writes.total_us += metric.total_us;
            writes.calls += metric.calls;
            writes.max_us = writes.max_us.max(metric.max_us);
        }
    }
    if writes.calls > 0 {
        out.push_str("磁盘写入（已计入上述耗时）\n");
        line(&mut out, "  持久化保存", &writes);
        show(&mut out, "store.file_fsync", "  文件同步");
        show(&mut out, "store.directory_fsync", "  目录同步");
    }
    out
}

#[cfg(test)]
#[path = "../tests/unit/test_timings.rs"]
pub(crate) mod tests;
