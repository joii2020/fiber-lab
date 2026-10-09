mod account;
mod accounts;
mod admin;
mod channel;
mod payments;
mod rpc;
mod store;
mod wallet;

use anyhow::{Context, Result, ensure};
use clap::{Args, CommandFactory, Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "./lsp-cli", about = "Fiber LSP 联调工具")]
struct Cli {
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
    /// 创建或恢复客户端账户
    CreateUser(ClientOptions),
    /// 查询当前账户信息、通道详情及本地恢复阶段
    Status(ClientOptions),
    /// 开通或恢复 CKB 私有通道（金额单位 CKB）
    OpenChannel(channel::OpenOptions),
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
    /// 将当前账户归档，保留身份与通道记录
    ArchiveAccount(AccountOptions),
    /// 只读列出当前及已归档的本地账户
    ListAccounts,
}

#[derive(Args)]
struct AccountOptions {
    #[arg(long, value_name = "ID")]
    account: Option<String>,
}

#[derive(Args)]
struct ClientOptions {
    /// list-accounts 列出的账户 ID；省略时使用默认账户
    #[arg(long, value_name = "ID")]
    account: Option<String>,
    /// 配置路径；默认 ${XDG_DATA_HOME:-$HOME/.local/share}/offckb-nodejs/devnet/fiber/nodes/3/config.yml
    #[arg(long, value_name = "PATH")]
    config: Option<PathBuf>,
}

#[derive(Subcommand)]
enum AdminCommand {
    /// 将 offckb 节点离线配置为 LSP，生成或复用 Biscuit 密钥
    InitLsp(admin::InitLsp),
    /// 离线签发管理凭证：read("lsp"); write("lsp"); read("node");
    CreateToken(admin::CreateToken),
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    if let Err(error) = run().await {
        eprintln!("错误：{error:#}");
        std::process::exit(1);
    }
}

async fn run() -> Result<()> {
    let Some(command) = Cli::parse().command else {
        Cli::command().print_help()?;
        println!();
        return Ok(());
    };
    let create = matches!(command, Command::CreateUser(_));
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
        Command::ListAccounts => accounts::list(&accounts::root())?,
        Command::ArchiveAccount(options) => {
            let id = accounts::archive(&accounts::root(), options.account.as_deref())?;
            println!("账户 {id} 已归档；身份、凭证及通道记录保留，未关闭通道或注销租户。");
            println!("查询：./lsp-cli status --account {id}");
            println!("关闭 / 恢复：./lsp-cli close-channel --account {id}");
            println!("创建默认账户：./lsp-cli create-user");
        }
        Command::Pay(options) => payments::pay(options).await?,
        Command::CreateInvoice(options) => payments::create_invoice(options).await?,
        Command::Watch(options) => payments::watch(options).await?,
        Command::Resume(options) => payments::resume(options).await?,
        Command::PaymentStatus(options) => payments::status(options, false).await?,
        Command::InvoiceStatus(options) => payments::status(options, true).await?,
        Command::ListPayments(options) => payments::list(options).await?,
        Command::OpenChannel(options) => channel::open(options).await?,
        Command::CloseChannel(options) => channel::close(options).await?,
        Command::CreateUser(options) | Command::Status(options) => {
            let config = rpc::Config::load(options.config.as_deref())?;
            let funding = default_funding_account()?;
            let account = account::run(&config, create, options.account.as_deref()).await?;
            println!("  {}", account.outcome);
            println!("  身份公钥：{}", account.identity);
            println!("  tenant_id：{}", account.tenant_id);
            println!("  通道数量：{}", channel::count(&account.channels));
            println!("\n默认出资账户：offckb #{}", funding.index);
            println!("  出资 lock args：{}", funding.lock_arg);
            println!("  出资 address：{}", funding.address);
            if !create {
                channel::print_details(&account.client, &account.channels).await?;
            }
        }
    }
    Ok(())
}

#[derive(serde::Deserialize)]
struct FundingAccount {
    index: usize,
    #[serde(rename = "lockArg")]
    lock_arg: String,
    address: String,
}

fn default_funding_account() -> Result<FundingAccount> {
    let output = std::process::Command::new("offckb")
        .args(["--json", "accounts"])
        .output()
        .context("无法执行 offckb --json accounts；请检查 offckb 是否已安装")?;
    ensure!(output.status.success(), "offckb --json accounts 执行失败");
    parse_funding_account(&output.stdout)
}

fn parse_funding_account(output: &[u8]) -> Result<FundingAccount> {
    #[derive(serde::Deserialize)]
    struct Accounts {
        accounts: Vec<FundingAccount>,
    }
    let result: Accounts =
        serde_json::from_slice(output).context("无法解析 offckb --json accounts 的账户信息")?;
    let account = result
        .accounts
        .into_iter()
        .find(|account| account.index == wallet::CLIENT_FUNDING_ACCOUNT)
        .context("offckb 账户列表中缺少默认出资账户 #6")?;
    ensure!(
        !account.lock_arg.is_empty() && !account.address.is_empty(),
        "offckb #6 的 lock args 或 address 为空"
    );
    Ok(account)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn funding_account_reads_values_by_index() {
        let account = parse_funding_account(
            br#"{"accounts":[{"index":5,"lockArg":"0xabcd","address":"lsp-address"},{"index":6,"lockArg":"0x1234","address":"runtime-address"},{"index":0,"lockArg":"0xabcd","address":"other-address"}]}"#,
        )
        .unwrap();
        assert_eq!(account.index, 6);
        assert_eq!(account.lock_arg, "0x1234");
        assert_eq!(account.address, "runtime-address");
        assert!(parse_funding_account(br#"{"accounts":[]}"#).is_err());
        assert!(
            parse_funding_account(br#"{"accounts":[{"index":6,"lockArg":"","address":""}]}"#)
                .is_err()
        );
        assert!(
            parse_funding_account(
                br#"{"accounts":[{"index":5,"lockArg":"0xabcd","address":"lsp-address"}]}"#
            )
            .is_err()
        );
        assert!(parse_funding_account(b"invalid json").is_err());
    }

    #[test]
    fn account_management_and_archived_close_parse() {
        assert!(matches!(
            Cli::try_parse_from(["lsp-cli", "list-accounts"])
                .unwrap()
                .command,
            Some(Command::ListAccounts)
        ));
        assert!(matches!(
            Cli::try_parse_from(["lsp-cli", "archive-account"])
                .unwrap()
                .command,
            Some(Command::ArchiveAccount(_))
        ));
        let Some(Command::CloseChannel(options)) =
            Cli::try_parse_from(["lsp-cli", "close-channel", "--account", "default"])
                .unwrap()
                .command
        else {
            panic!("close command expected")
        };
        assert_eq!(options.account.as_deref(), Some("default"));
    }

    #[test]
    fn help_and_client_commands_parse_without_loading_files() {
        for args in [
            vec!["lsp-cli", "--help"],
            vec!["lsp-cli", "admin", "--help"],
            vec!["lsp-cli", "admin", "init-lsp", "--help"],
            vec!["lsp-cli", "admin", "create-token", "--help"],
        ] {
            assert_eq!(
                Cli::try_parse_from(args).err().unwrap().kind(),
                clap::error::ErrorKind::DisplayHelp
            );
        }
        assert!(matches!(
            Cli::try_parse_from(["lsp-cli", "create-user"])
                .unwrap()
                .command,
            Some(Command::CreateUser(_))
        ));
        assert!(matches!(
            Cli::try_parse_from(["lsp-cli", "status"]).unwrap().command,
            Some(Command::Status(_))
        ));
    }
}
