//! Sequential payment coordinator. Stores public identifiers and timing summaries only.
use std::{fs::OpenOptions, io::Write, path::PathBuf, process::Stdio, time::Instant};

use anyhow::{Context, Result, bail, ensure};
use clap::Parser;
use serde_json::{Value, json};
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    process::{Child, Command},
    time::{Duration, sleep, timeout},
};

#[derive(Parser)]
struct Options {
    /// Fresh JSONL destination; existing files are never replaced.
    #[arg(long)]
    output: PathBuf,
    /// Exact CLI/FNN commits, release profile, and instrumented/uninstrumented build label.
    #[arg(long)]
    build: String,
    /// Explicit scenario/group label (cold CLI, warm runtime, history count, etc.).
    #[arg(long)]
    group: String,
    #[arg(long, default_value_t = 30)]
    samples: usize,
    #[arg(long, default_value_t = 5)]
    warmups: usize,
    #[arg(long, default_value_t = 120)]
    timeout: u64,
    /// CLI compiled from an isolated checkout, never the existing account checkout.
    #[arg(long, requires = "config", conflicts_with = "normal_payer")]
    cli: Option<PathBuf>,
    #[arg(long)]
    config: Option<PathBuf>,
    #[arg(long, default_value = "1")]
    payer: String,
    #[arg(long, default_value = "2")]
    receiver: String,
    /// Collect CLI phase metrics; run the same scenario without this flag to measure overhead.
    #[arg(long)]
    timings: bool,
    /// Ordinary Fiber RPC endpoints; channels/liquidity must already be ready.
    #[arg(long, requires = "normal_receiver")]
    normal_payer: Option<String>,
    #[arg(long)]
    normal_receiver: Option<String>,
    #[arg(long, requires = "normal_payer")]
    hold: bool,
}

fn cli(options: &Options, args: &[&str], user: &str) -> Result<Child> {
    let mut command = Command::new(options.cli.as_ref().context("missing --cli")?);
    command
        .args(args)
        .args(["-u", user, "--config"])
        .arg(options.config.as_ref().context("missing --config")?)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    if options.timings {
        command.arg("--timings-json");
    }
    Ok(command.spawn()?)
}

async fn lsp(options: &Options) -> Result<Value> {
    let seconds = options.timeout.to_string();
    let mut receiver = cli(
        options,
        &[
            "create-invoice",
            "--amount",
            "1",
            "--wait",
            "--timeout",
            &seconds,
        ],
        &options.receiver,
    )?;
    let mut lines = BufReader::new(receiver.stdout.take().context("missing stdout")?).lines();
    let mut invoice = None;
    let mut hash = None;
    // Never save these lines: invoice/preimage/recovery output is not benchmark data.
    while invoice.is_none() || hash.is_none() {
        let line = lines
            .next_line()
            .await?
            .context("receiver exited before invoice readiness")?;
        if let Some(value) = line.strip_prefix("invoice_address：") {
            invoice = Some(value.to_owned());
        }
        if let Some(value) = line.strip_prefix("付款 hash：") {
            hash = Some(value.to_owned());
        }
    }
    // The invoice exists remotely before readiness is announced; receiver stays online.
    let start = Instant::now();
    let payer = cli(
        options,
        &[
            "pay",
            invoice.as_deref().unwrap(),
            "--max-fee",
            "0.01",
            "--timeout",
            &seconds,
        ],
        &options.payer,
    )?;
    let payer_future = async {
        let result = payer.wait_with_output().await?;
        Ok::<_, anyhow::Error>((start.elapsed().as_micros(), result))
    };
    let receiver_future = async {
        let mut complete = false;
        while let Some(line) = lines.next_line().await? {
            complete |= line.contains("收款完成（远端状态、SDK TLC 移除及余额已核对）");
        }
        let result = receiver.wait_with_output().await?;
        Ok::<_, anyhow::Error>((start.elapsed().as_micros(), complete, result))
    };
    let (payer_result, receiver_result) = tokio::join!(payer_future, receiver_future);
    let (payer_us, payer) = payer_result?;
    let (receiver_us, receiver_complete, receiver) = receiver_result?;
    let payer_complete = String::from_utf8_lossy(&payer.stdout)
        .contains("付款完成（远端状态、SDK TLC 移除及余额已核对）");
    let success =
        payer.status.success() && receiver.status.success() && payer_complete && receiver_complete;
    Ok(
        json!({"hash": hash, "success": success, "failure": if success { Value::Null } else { json!("cli_incomplete_or_error") },
        "payer_complete_us": payer_us, "receiver_complete_us": receiver_us,
        "payer_metrics": metrics(&payer.stderr), "receiver_metrics": metrics(&receiver.stderr),
        "receiver_readiness": "invoice_created_receiver_process_online", "receiver_balance_us": null,
        "payment_success_us": null, "server_exact_timing": false}),
    )
}

fn metrics(stderr: &[u8]) -> Option<Value> {
    String::from_utf8_lossy(stderr).lines().find_map(|line| {
        line.strip_prefix("timings ")
            .and_then(|v| serde_json::from_str(v).ok())
    })
}

async fn rpc(client: &reqwest::Client, url: &str, method: &str, params: Value) -> Result<Value> {
    let result: Value = client
        .post(url)
        .json(&json!({"jsonrpc":"2.0","id":1,"method":method,"params":[params]}))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    // Remote error strings may contain sensitive payloads; classify instead of copying.
    ensure!(result.get("error").is_none(), "rpc_error:{method}");
    result.get("result").cloned().context("missing_rpc_result")
}

async fn normal(options: &Options) -> Result<Value> {
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(10))
        .build()?;
    let payer = options
        .normal_payer
        .as_deref()
        .context("missing --normal-payer")?;
    let receiver = options
        .normal_receiver
        .as_deref()
        .context("missing --normal-receiver")?;
    let preimage = rand::random::<[u8; 32]>();
    let hash = format!("0x{}", hex::encode(ckb_hash::blake2b_256(preimage)));
    let mut params = json!({"amount":"0x5f5e100","currency":"Fibd","hash_algorithm":"ckb_hash"});
    if options.hold {
        params["payment_hash"] = json!(hash);
    }
    let invoice = rpc(&client, receiver, "new_invoice", params).await?;
    let invoice = invoice["invoice_address"]
        .as_str()
        .context("missing_invoice")?;
    let start = Instant::now();
    let sent = rpc(
        &client,
        payer,
        "send_payment",
        json!({"invoice":invoice,"max_fee_amount":"0xf4240"}),
    )
    .await?;
    let send_us = start.elapsed().as_micros();
    let hash = sent["payment_hash"].as_str().context("missing_hash")?;
    let mut released = !options.hold;
    let mut release_us = None;
    let mut polls = 0;
    loop {
        polls += 1;
        if !released {
            let status = rpc(
                &client,
                receiver,
                "get_invoice",
                json!({"payment_hash":hash}),
            )
            .await?;
            if status["status"] == "Received" {
                let release = Instant::now();
                rpc(&client, receiver, "settle_invoice", json!({"payment_hash":hash,"payment_preimage":format!("0x{}",hex::encode(preimage))})).await?;
                release_us = Some(release.elapsed().as_micros());
                released = true;
            }
        }
        let payment = rpc(&client, payer, "get_payment", json!({"payment_hash":hash})).await?;
        let status = payment["status"].as_str().context("missing_status")?;
        if matches!(status, "Success" | "Failed") {
            return Ok(
                json!({"hash":hash,"success":status == "Success","failure":if status == "Failed" {json!("remote_failed")} else {Value::Null},
                "send_rpc_us":send_us,"payer_success_observed_us":start.elapsed().as_micros(),
                "observation_poll_interval_us":2000,"polls":polls,"release_rpc_us":release_us,"server_exact_timing":false}),
            );
        }
        sleep(Duration::from_millis(2)).await;
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    let options = Options::parse();
    ensure!(
        options.samples > 0 && options.timeout > 0,
        "samples and timeout must be positive"
    );
    ensure!(
        options.cli.is_some() || options.normal_payer.is_some(),
        "choose --cli or --normal-payer"
    );
    let mut output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&options.output)?;
    let mut successful = Vec::new();
    let mut failures = 0;
    for attempt in 0..options.warmups + options.samples {
        let start = Instant::now();
        let run = async {
            if options.cli.is_some() {
                lsp(&options).await
            } else {
                normal(&options).await
            }
        };
        let mut record = match timeout(Duration::from_secs(options.timeout + 10), run).await {
            Ok(Ok(record)) => record,
            Ok(Err(_)) => json!({"success":false,"failure":"coordinator_or_rpc_error"}),
            Err(_) => json!({"success":false,"failure":"timeout"}),
        };
        let warmup = attempt < options.warmups;
        record["attempt"] = json!(attempt);
        record["warmup"] = json!(warmup);
        record["build"] = json!(options.build);
        record["group"] = json!(options.group);
        record["wall_us"] = json!(start.elapsed().as_micros());
        writeln!(output, "{record}")?;
        output.flush()?;
        if record["success"] != true {
            failures += usize::from(!warmup);
            // A new invoice after an unknown outcome would obscure recovery and liquidity.
            // Preserve the failed attempt and stop instead of issuing another payment.
            bail!(
                "attempt {attempt} incomplete; measured failures: {failures}; recover its original hash before continuing; result saved"
            );
        }
        if !warmup {
            let duration = record
                .get("payer_complete_us")
                .or_else(|| record.get("payer_success_observed_us"))
                .and_then(Value::as_u64)
                .context("missing_duration")?;
            successful.push(duration);
        }
    }
    successful.sort_unstable();
    println!(
        "{}",
        json!({"samples":successful.len(),"failures":failures,"p50_us":successful[(successful.len()-1)/2],"p95_us":successful[(successful.len()*95).div_ceil(100)-1]})
    );
    Ok(())
}
