//! Opt-in, payload-free timings for the current-thread CLI runtime.
use std::{cell::RefCell, collections::BTreeMap, time::Instant};

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

pub fn report(success: bool) {
    STATE.with_borrow(|state| {
        if state.enabled {
            eprintln!(
                "timings {}",
                serde_json::json!({
                    "schema": 1, "unit": "us", "success": success,
                    "profile": if cfg!(debug_assertions) { "debug" } else { "release" },
                    "metrics": state.metrics,
                })
            );
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
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
        drop(Span::rpc("lsp", "wait_channel_update"));
        STATE.with_borrow(|s| {
            assert_eq!(s.metrics["rpc.lsp.success"].calls, 1);
            assert_eq!(s.metrics["rpc.ckb.failed"].errors, 1);
            assert_eq!(s.metrics["rpc.lsp.wait_channel_update"].cancelled, 1);
        });
        enable(false);
    }
}
