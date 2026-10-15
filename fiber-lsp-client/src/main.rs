mod account;
mod accounts;
mod admin;
mod channel;
mod payments;
mod rpc;
mod store;
mod timings;
mod wallet;

use anyhow::{Result, ensure};
use clap::{Args, CommandFactory, Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "./lsp-cli", about = "Fiber LSP 联调工具")]
struct Cli {
    /// 将中文分阶段耗时摘要写入 stderr
    #[arg(long, global = true, conflicts_with = "timings_json")]
    timings: bool,
    /// 将完整计时 JSON 写入 stderr，供自动基准使用
    #[arg(long, global = true, conflicts_with = "timings")]
    timings_json: bool,
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// LSP 管理操作（显式读取服务端私钥）
    Admin {
        #[command(subcommand)]
        command: AdminCommand,
    },
    /// 查询当前账户信息、通道详情及本地恢复阶段
    Status(ClientOptions),
    /// 开通或恢复 CKB 私有通道（金额单位 CKB）
    CreateChannel(channel::OpenOptions),
    /// 协作关闭当前账户通道并核对链上退款
    CloseChannel(channel::CloseOptions),
    /// 按原授权付款并等待结算（金额及费用单位 CKB）
    Pay(payments::PayOptions),
    /// 创建 hold invoice；默认输出后退出，--wait 在线收款
    CreateInvoice(payments::InvoiceOptions),
    /// 前台推进当前账户已有收付款授权
    Watch(ClientOptions),
    /// 按原付款 hash 有界恢复收付款
    Resume(payments::ResumeOptions),
    /// 只读核对本地及远端付款状态
    PaymentStatus(payments::HashOptions),
    /// 只读核对本地及远端发票状态
    InvoiceStatus(payments::HashOptions),
    /// 只读列出本地收付款记录
    ListPayments(ClientOptions),
    /// 离线列出四个固定账户；--history 同时显示各账户历轮及旧身份
    Accounts {
        #[arg(long)]
        history: bool,
    },
}

#[derive(Args)]
#[group(required = true, multiple = false)]
struct Selector {
    /// 固定账户编号 1～4
    #[arg(short = 'u', long, value_parser = clap::value_parser!(u8).range(1..=4))]
    user: Option<u8>,
    /// 兼容旧数据的内部身份 ID；日常历史操作使用 -u N --round R
    #[arg(long, value_name = "ID")]
    account: Option<String>,
}

#[derive(Args)]
struct ClientOptions {
    #[command(flatten)]
    selector: Selector,
    /// 选择固定账户的历史轮次（accounts --history 展示）
    #[arg(long, value_name = "R", value_parser = clap::value_parser!(u32).range(1..), requires = "user", conflicts_with = "account")]
    round: Option<u32>,
    /// Node3 配置路径
    #[arg(long, value_name = "PATH")]
    config: Option<PathBuf>,
}
impl std::ops::Deref for ClientOptions {
    type Target = Selector;
    fn deref(&self) -> &Selector {
        &self.selector
    }
}
impl ClientOptions {
    fn current_only(&self) -> Result<()> {
        ensure!(
            self.user.is_some() && self.round.is_none(),
            "本命令必须使用 -u N 当前轮次；--round 和 --account 仅用于历史查询和恢复"
        );
        Ok(())
    }
    fn recovery_selector(&self) -> String {
        self.user.map_or_else(
            || format!("--account {}", self.account.as_deref().unwrap()),
            |u| {
                self.round
                    .map_or_else(|| format!("-u {u}"), |r| format!("-u {u} --round {r}"))
            },
        )
    }
}

#[derive(Subcommand)]
enum AdminCommand {
    /// 将 offckb 节点离线配置为 LSP，生成或复用 Biscuit 密钥
    InitLsp(admin::InitLsp),
    /// 离线创建四用户及首轮身份；复用既有绑定和历史
    InitAccounts(admin::InitAccounts),
    /// 离线签发管理凭证：read("lsp"); write("lsp"); read("node");
    CreateToken(admin::CreateToken),
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let cli = Cli::parse();
    let timings_json = cli.timings_json;
    timings::enable(cli.timings || timings_json);
    let total = timings::Span::new("cli.total");
    let result = run(cli).await;
    drop(total);
    timings::report(result.is_ok(), timings_json);
    if let Err(error) = result {
        eprintln!("错误：{error:#}");
        std::process::exit(1);
    }
}

async fn run(cli: Cli) -> Result<()> {
    let Some(command) = cli.command else {
        Cli::command().print_help()?;
        println!();
        return Ok(());
    };
    match command {
        Command::Admin {
            command: AdminCommand::InitLsp(options),
        } => {
            let initialized = admin::init_lsp(&options)?;
            println!("LSP 配置已保存：{}", initialized.config.display());
            println!("Biscuit 私钥文件：{}", initialized.private_key.display());
            println!("初始化完成；已有密钥复用，未启动节点或签发管理凭证。");
        }
        Command::Admin {
            command: AdminCommand::CreateToken(options),
        } => {
            let created = admin::create_token(&options)?;
            println!("配置：{}", created.config.display());
            println!("管理凭证已保存：{}", created.output.display());
            println!("权限：{}", admin::PERMISSIONS);
            println!("签发完成；重启时复用该凭证。");
        }
        Command::Admin {
            command: AdminCommand::InitAccounts(options),
        } => {
            admin::init_accounts(&options).await?;
        }
        Command::Accounts { history } => accounts::list(&accounts::root(), history)?,
        Command::Pay(options) => payments::pay(options).await?,
        Command::CreateInvoice(options) => payments::create_invoice(options).await?,
        Command::Watch(options) => payments::watch(options).await?,
        Command::Resume(options) => payments::resume(options).await?,
        Command::PaymentStatus(options) => payments::status(options, false).await?,
        Command::InvoiceStatus(options) => payments::status(options, true).await?,
        Command::ListPayments(options) => payments::list(options).await?,
        Command::CreateChannel(options) => channel::open(options).await?,
        Command::CloseChannel(options) => channel::close(options).await?,
        Command::Status(options) => {
            let config = rpc::Config::load(options.config.as_deref())?;
            let (dir, _selection) = accounts::choose(&config.root, &options, false, true, false)?;
            account::verify_local(&dir, &config).await?;
            let local: serde_json::Value = dir.read_json()?;
            println!("本地注册阶段：{}", local["registration"]);
            if local["registration"] != "registered" {
                println!("tenant_id：{}", local["tenant_id"]);
                println!("尚未注册；通过 create-channel -u N 创建或恢复，不改变本地身份");
                return Ok(());
            }
            let account = account::run_dir(&config, false, dir).await?;
            println!(
                "{}\n身份公钥：{}\ntenant_id：{}\n通道数量：{}",
                account.outcome,
                account.identity,
                account.tenant_id,
                channel::count(&account.channels)
            );
            channel::print_details(&account.client, &account.channels).await?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn user_selection_is_required_exclusive_and_bounded() {
        for command in [
            "status",
            "create-channel",
            "close-channel",
            "watch",
            "list-payments",
        ] {
            assert!(Cli::try_parse_from(["lsp-cli", command]).is_err());
            for user in ["0", "5"] {
                assert!(Cli::try_parse_from(["lsp-cli", command, "-u", user]).is_err());
            }
            assert!(
                Cli::try_parse_from(["lsp-cli", command, "-u", "1", "--account", "default"])
                    .is_err()
            );
            assert!(Cli::try_parse_from(["lsp-cli", command, "-u", "4"]).is_ok());
        }
        for old in [
            "create-user",
            "open-channel",
            "archive-account",
            "list-accounts",
        ] {
            assert!(Cli::try_parse_from(["lsp-cli", old]).is_err());
        }
        assert!(Cli::try_parse_from(["lsp-cli", "accounts", "--history"]).is_ok());
        assert!(Cli::try_parse_from(["lsp-cli", "admin", "init-accounts"]).is_ok());
    }
    #[test]
    fn restored_funding_amount_stays_optional() {
        let Some(Command::CreateChannel(options)) =
            Cli::try_parse_from(["lsp-cli", "create-channel", "-u", "1"])
                .unwrap()
                .command
        else {
            panic!()
        };
        assert!(options.funding_amount.is_none());
    }
    #[test]
    fn historical_round_requires_fixed_account_and_recovery_preserves_round() {
        for command in ["status", "close-channel", "list-payments"] {
            assert!(Cli::try_parse_from(["lsp-cli", command, "--round", "1"]).is_err());
            assert!(Cli::try_parse_from(["lsp-cli", command, "-u", "1", "--round", "0"]).is_err());
            assert!(
                Cli::try_parse_from(["lsp-cli", command, "--account", "default", "--round", "1"])
                    .is_err()
            );
            assert!(Cli::try_parse_from(["lsp-cli", command, "-u", "1", "--round", "1"]).is_ok());
        }
        assert!(
            Cli::try_parse_from(["lsp-cli", "create-channel", "-u", "1", "--round", "1"]).is_err()
        );
        let Some(Command::Status(options)) =
            Cli::try_parse_from(["lsp-cli", "status", "-u", "2", "--round", "3"])
                .unwrap()
                .command
        else {
            panic!()
        };
        assert_eq!(options.recovery_selector(), "-u 2 --round 3");
        assert!(options.current_only().is_err());
    }
}
