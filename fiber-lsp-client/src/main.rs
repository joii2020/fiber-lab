mod account;
mod accounts;
mod admin;
mod channel;
mod events;
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
    /// 在线支付一张发票并等待结算（金额及费用单位 CKB）
    Pay(payments::PayOptions),
    /// 创建发票并保持在线，等待一笔收款完成
    CreateInvoice(payments::InvoiceOptions),
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

#[derive(Args, Default, Clone)]
struct ConnectionOptions {
    /// LSP RPC 地址（每次调用必须指定）
    #[arg(long, value_name = "URL")]
    rpc_url: String,
    /// 覆盖默认管理凭证；仅首次注册或开通时使用
    #[arg(long, value_name = "TOKEN", conflicts_with = "token_file")]
    token: Option<String>,
    /// 从文件覆盖管理凭证；默认 configs/keys/operator-token
    #[arg(long, value_name = "PATH", conflicts_with = "token")]
    token_file: Option<PathBuf>,
}
impl ConnectionOptions {
    fn read_token(&self) -> Result<Option<zeroize::Zeroizing<String>>> {
        let token = match (&self.token, &self.token_file) {
            (Some(token), _) => Some(token.clone()),
            (_, Some(path)) => Some(
                std::fs::read_to_string(path)
                    .map_err(|_| anyhow::anyhow!("无法读取 token 文件 {}", path.display()))?,
            ),
            _ => None,
        };
        token
            .map(|token| {
                let token = zeroize::Zeroizing::new(token);
                ensure!(!token.trim().is_empty(), "token 不能为空");
                Ok(zeroize::Zeroizing::new(token.trim().to_owned()))
            })
            .transpose()
    }
}

#[derive(Args)]
struct CurrentClientOptions {
    /// 固定账户编号 1～4（当前轮次）
    #[arg(short = 'u', long, value_parser = clap::value_parser!(u8).range(1..=4))]
    user: u8,
    /// LSP 独立 WebSocket RPC 地址，对应 rpc.ws_listening_addr
    #[arg(long, default_value = "ws://127.0.0.1:21717", value_name = "URL", value_parser = events::parse_url)]
    ws_url: String,
    #[command(flatten)]
    connection: ConnectionOptions,
}
impl From<CurrentClientOptions> for ClientOptions {
    fn from(options: CurrentClientOptions) -> Self {
        Self {
            selector: Selector {
                user: Some(options.user),
                account: None,
            },
            round: None,
            connection: options.connection,
        }
    }
}

#[derive(Args)]
struct ClientOptions {
    #[command(flatten)]
    selector: Selector,
    /// 选择固定账户的历史轮次（accounts --history 展示）
    #[arg(long, value_name = "R", value_parser = clap::value_parser!(u32).range(1..), requires = "user", conflicts_with = "account")]
    round: Option<u32>,
    #[command(flatten)]
    connection: ConnectionOptions,
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
        Command::CreateChannel(options) => channel::open(options).await?,
        Command::CloseChannel(options) => channel::close(options).await?,
        Command::Status(options) => {
            let (dir, _selection) =
                accounts::choose(&accounts::root(), &options, false, true, false)?;
            let config = rpc::Connection::connect(&options.connection)?;
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
#[path = "test_main.rs"]
mod tests;
