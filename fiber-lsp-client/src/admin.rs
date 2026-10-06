use std::{
    env,
    fs::{self, OpenOptions},
    io::Read,
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::PathBuf,
    str::FromStr,
};

use anyhow::{Context, Result, bail};
use biscuit_auth::{Biscuit, KeyPair, PrivateKey, PublicKey};
use clap::Args;
use serde::Deserialize;
use zeroize::Zeroizing;

use crate::store;

pub const PERMISSIONS: &str = "read(\"lsp\"); write(\"lsp\"); read(\"node\");";

#[derive(Args)]
#[command(
    after_help = "默认离线读取 offckb Node3 的实际配置，无需启动 LSP 或复制 configs/lsp.yml。\n相对路径均相对于当前工作目录。私钥必须是仅当前用户可访问的普通文件。\n目标已存在（含符号链接）时拒绝覆盖。权限不包含 write(\"node\")。"
)]
pub struct CreateToken {
    /// 配置路径；默认 ${XDG_DATA_HOME:-$HOME/.local/share}/offckb-nodejs/devnet/fiber/nodes/3/config.yml
    #[arg(long, value_name = "PATH")]
    pub config: Option<PathBuf>,
    /// 私钥文件；默认读取配置 rpc.biscuit_private_key_path，显式指定时覆盖该字段
    #[arg(long, value_name = "PATH")]
    pub private_key_file: Option<PathBuf>,
    /// 输出文件；默认项目内 configs/keys/operator-token（首次创建，权限 600）
    #[arg(long, value_name = "PATH")]
    pub output: Option<PathBuf>,
}

pub struct CreatedToken {
    pub config: PathBuf,
    pub output: PathBuf,
}

fn output_path(options: &CreateToken) -> Result<PathBuf> {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .context("无法定位 fiber-lab 项目目录")?;
    Ok(options
        .output
        .clone()
        .unwrap_or_else(|| root.join("configs/keys/operator-token")))
}

fn node3_config(
    xdg: Option<std::ffi::OsString>,
    home: Option<std::ffi::OsString>,
) -> Result<PathBuf> {
    let data = match xdg.filter(|value| !value.is_empty()) {
        Some(value) => PathBuf::from(value),
        None => PathBuf::from(
            home.filter(|value| !value.is_empty())
                .context("未设置 HOME；请指定 --config")?,
        )
        .join(".local/share"),
    };
    Ok(data.join("offckb-nodejs/devnet/fiber/nodes/3/config.yml"))
}

pub fn create_token(options: &CreateToken) -> Result<CreatedToken> {
    let config = match &options.config {
        Some(path) => path.clone(),
        None => node3_config(env::var_os("XDG_DATA_HOME"), env::var_os("HOME"))?,
    };
    let config = std::path::absolute(config)?;
    let output = std::path::absolute(output_path(options)?)?;

    #[derive(Deserialize)]
    struct Config {
        rpc: Rpc,
    }
    #[derive(Deserialize)]
    struct Rpc {
        biscuit_public_key: String,
        biscuit_private_key_path: Option<PathBuf>,
    }
    let bytes = fs::read(&config).with_context(|| {
        format!(
            "无法读取配置 {}；请检查路径或指定 --config",
            config.display()
        )
    })?;
    // YAML errors may include source lines; do not attach them to diagnostics.
    let parsed: Config = serde_yaml::from_slice(&bytes).map_err(|_| {
        anyhow::anyhow!(
            "{} 的 YAML 无效或缺少 rpc.biscuit_public_key；请补齐配置",
            config.display()
        )
    })?;
    let public = PublicKey::from_str(parsed.rpc.biscuit_public_key.trim()).map_err(|_| {
        anyhow::anyhow!(
            "{} 的 rpc.biscuit_public_key 无效；请填写完整 Biscuit 公钥",
            config.display()
        )
    })?;
    let key_path = options
        .private_key_file
        .clone()
        .or(parsed.rpc.biscuit_private_key_path)
        .filter(|path| !path.as_os_str().is_empty())
        .context("缺少 rpc.biscuit_private_key_path；请补齐配置或指定 --private-key-file")?;
    let key_path = std::path::absolute(key_path)?;
    // Open without following a symlink and validate the opened file, avoiding a check/read race.
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(&key_path)
        .with_context(|| {
            format!(
                "无法读取私钥 {}；请检查路径，私钥必须是普通文件，不能是符号链接",
                key_path.display()
            )
        })?;
    let metadata = file.metadata()?;
    // SAFETY: geteuid takes no arguments and has no memory safety preconditions.
    let uid = unsafe { libc::geteuid() };
    if !metadata.is_file() || metadata.uid() != uid || metadata.mode() & 0o077 != 0 {
        bail!(
            "私钥 {} 必须由当前用户拥有且仅当前用户可访问的普通文件；请核对所有者并执行 chmod 600；原文件已保留",
            key_path.display()
        );
    }
    let mut text = Zeroizing::new(String::new());
    file.read_to_string(&mut text).with_context(|| {
        format!(
            "无法读取私钥 {} 的文本；请保存完整 ed25519-private/... 字符串",
            key_path.display()
        )
    })?;
    let private_text = text.trim();
    if !private_text.starts_with("ed25519-private/") {
        bail!(
            "私钥格式无效；请在 {} 保存完整 ed25519-private/... 字符串",
            key_path.display()
        );
    }
    let private = PrivateKey::from_str(private_text).map_err(|_| {
        anyhow::anyhow!(
            "私钥格式无效；请在 {} 保存完整 ed25519-private/... 字符串",
            key_path.display()
        )
    })?;
    let root = KeyPair::from(&private);
    if root.public() != public {
        bail!(
            "私钥与配置 rpc.biscuit_public_key 不匹配；请核对 {} 和 {}",
            key_path.display(),
            config.display()
        );
    }
    let token = Zeroizing::new(
        Biscuit::builder()
            .code(PERMISSIONS)?
            .build(&root)?
            .to_base64()
            .context("无法编码管理凭证")?,
    );
    store::create_private(&output, token.as_bytes())?;
    Ok(CreatedToken { config, output })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_output_matches_client_operator_token_location() {
        let options = CreateToken {
            config: None,
            private_key_file: None,
            output: None,
        };
        let client_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap();
        assert_eq!(
            output_path(&options).unwrap(),
            client_root.join("configs/keys/operator-token")
        );
    }

    #[test]
    fn default_config_needs_home_only_without_xdg() {
        assert_eq!(
            node3_config(Some("/data".into()), None).unwrap(),
            PathBuf::from("/data/offckb-nodejs/devnet/fiber/nodes/3/config.yml")
        );
        assert!(
            node3_config(None, None)
                .unwrap_err()
                .to_string()
                .contains("--config")
        );
    }
}
