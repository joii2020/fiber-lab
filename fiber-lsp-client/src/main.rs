mod account;
mod admin;
mod channel;
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
    /// 使用用户凭证查询当前账户的通道数量
    Status(ClientOptions),
    /// 开通或恢复 CKB 私有通道（金额单位 CKB）
    #[command(name = "openchannel", visible_alias = "open-channel")]
    OpenChannel(channel::OpenOptions),
    /// 协作关闭当前账户通道并核对链上退款
    #[command(name = "closechannel", visible_alias = "close-channel")]
    CloseChannel(channel::CloseOptions),
    /// 只读查询当前账户的所有通道及本地恢复阶段
    #[command(name = "listChannel")]
    ListChannel(ClientOptions),
}

#[derive(Args)]
struct ClientOptions {
    /// 配置路径；默认 ${XDG_DATA_HOME:-$HOME/.local/share}/offckb-nodejs/devnet/fiber/nodes/3/config.yml
    #[arg(long, value_name = "PATH")]
    config: Option<PathBuf>,
}

#[derive(Subcommand)]
enum AdminCommand {
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
            command: AdminCommand::CreateToken(options),
        } => {
            let created = admin::create_token(&options)?;
            println!("配置：{}", created.config.display());
            println!("管理凭证已保存：{}", created.output.display());
            println!("权限：{}", admin::PERMISSIONS);
            println!("签发完成；重启时复用该凭证。");
        }
        Command::OpenChannel(options) => channel::open(options).await?,
        Command::CloseChannel(options) => channel::close(options).await?,
        Command::ListChannel(options) => {
            let config = rpc::Config::load(options.config.as_deref())?;
            channel::list(&config).await?;
        }
        Command::CreateUser(options) | Command::Status(options) => {
            let config = rpc::Config::load(options.config.as_deref())?;
            let funding = default_funding_account()?;
            let account = account::run(&config, create).await?;
            println!("{}", account.outcome);
            println!("身份公钥：{}", account.identity);
            println!("tenant_id：{}", account.tenant_id);
            println!("通道数量：{}", account.channel_count);
            println!("默认出资账户：offckb #{}", funding.index);
            println!("出资 lock args：{}", funding.lock_arg);
            println!("出资 address：{}", funding.address);
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
        .find(|account| account.index == 5)
        .context("offckb 账户列表中缺少默认出资账户 #5")?;
    ensure!(
        !account.lock_arg.is_empty() && !account.address.is_empty(),
        "offckb #5 的 lock args 或 address 为空"
    );
    Ok(account)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn funding_account_reads_values_by_index() {
        let account = parse_funding_account(
            br#"{"accounts":[{"index":5,"lockArg":"0x1234","address":"runtime-address"},{"index":0,"lockArg":"0xabcd","address":"other-address"}]}"#,
        )
        .unwrap();
        assert_eq!(account.index, 5);
        assert_eq!(account.lock_arg, "0x1234");
        assert_eq!(account.address, "runtime-address");
        assert!(parse_funding_account(br#"{"accounts":[]}"#).is_err());
        assert!(
            parse_funding_account(br#"{"accounts":[{"index":5,"lockArg":"","address":""}]}"#)
                .is_err()
        );
        assert!(parse_funding_account(b"invalid json").is_err());
    }

    #[test]
    fn help_and_client_commands_parse_without_loading_files() {
        for args in [
            vec!["lsp-cli", "--help"],
            vec!["lsp-cli", "admin", "--help"],
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
