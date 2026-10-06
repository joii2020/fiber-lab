mod account;
mod admin;
mod rpc;
mod store;

use anyhow::Result;
use clap::{CommandFactory, Parser, Subcommand};

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
    CreateUser,
    /// 使用用户凭证查询当前账户的通道数量
    Status,
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
        command @ (Command::CreateUser | Command::Status) => {
            let config = rpc::Config::load()?;
            let account = account::run(&config, matches!(command, Command::CreateUser)).await?;
            println!("{}", account.outcome);
            println!("身份公钥：{}", account.identity);
            println!("tenant_id：{}", account.tenant_id);
            println!("通道数量：{}", account.channel_count);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

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
            Some(Command::CreateUser)
        ));
        assert!(matches!(
            Cli::try_parse_from(["lsp-cli", "status"]).unwrap().command,
            Some(Command::Status)
        ));
    }
}
