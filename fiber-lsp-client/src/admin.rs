use std::{
    fs::{self, OpenOptions},
    io::{Read, Write},
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
    str::FromStr,
};

use anyhow::{Context, Result, bail, ensure};
use biscuit_auth::{Biscuit, KeyPair, PrivateKey, PublicKey};
use clap::Args;
use serde::Deserialize;
use serde_yaml::{Mapping, Value};
use zeroize::Zeroizing;

use crate::store;

pub const PERMISSIONS: &str = "read(\"lsp\"); write(\"lsp\"); read(\"node\");";

#[derive(Args)]
#[command(
    after_help = "离线修改 offckb 已生成的节点配置，不启动节点。\n保留其他配置字段；YAML 注释和排版会重新生成。已有密钥复用，冲突时拒绝修改。"
)]
pub struct InitLsp {
    /// 配置路径；默认 offckb Node3 的 config.yml
    #[arg(long, value_name = "PATH")]
    pub config: Option<PathBuf>,
}

pub struct InitializedLsp {
    pub config: PathBuf,
    pub private_key: PathBuf,
}

pub fn init_lsp(options: &InitLsp) -> Result<InitializedLsp> {
    let config = crate::rpc::config_path(options.config.as_deref())?;
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(&config)
        .with_context(|| {
            format!(
                "无法读取配置 {}；请先用 offckb 初始化节点",
                config.display()
            )
        })?;
    let metadata = file.metadata()?;
    ensure!(
        metadata.is_file(),
        "配置必须是普通文件，不能是符号链接或目录"
    );
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    let mut document: Value = serde_yaml::from_slice(&bytes)
        .map_err(|_| anyhow::anyhow!("{} 的 YAML 无效；原配置已保留", config.display()))?;
    let root = document
        .as_mapping_mut()
        .context("配置必须是 YAML mapping")?;
    add_to_list(root.get_mut("services"), &["lsp"], "services")?;
    let fiber = root
        .get_mut("fiber")
        .and_then(Value::as_mapping_mut)
        .context("缺少 fiber mapping；请使用 offckb 生成的配置")?;
    fiber.insert("announced_node_name".into(), "fiber-lab-lsp".into());
    // A lab channel must support receiving as well as sending. Preserve operator choices.
    let funding = fiber
        .entry(Value::from("auto_accept_channel_ckb_funding_amount"))
        .or_insert(Value::Null);
    if funding.is_null() {
        *funding = Value::from(1000 * crate::wallet::CKB);
    }

    let rpc = root
        .get_mut("rpc")
        .and_then(Value::as_mapping_mut)
        .context("缺少 rpc mapping；请使用 offckb 生成的配置")?;
    add_to_list(
        rpc.get_mut("enabled_modules"),
        &["lsp", "pubsub"],
        "rpc.enabled_modules",
    )?;
    let public_text = optional_string(rpc, "biscuit_public_key")?;
    let public = public_text
        .map(|text| {
            PublicKey::from_str(text)
                .map_err(|_| anyhow::anyhow!("rpc.biscuit_public_key 无效；原配置已保留"))
        })
        .transpose()?;
    let configured_path = optional_string(rpc, "biscuit_private_key_path")?;
    let key_path = std::path::absolute(match configured_path {
        Some(path) => PathBuf::from(path),
        None => config
            .parent()
            .context("配置路径缺少父目录")?
            .join("biscuit-private-key"),
    })?;
    let (key, new_key) = match fs::symlink_metadata(&key_path) {
        Ok(_) => (read_keypair(&key_path)?, false),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            ensure!(
                public.is_none() && configured_path.is_none(),
                "配置中已有 Biscuit 密钥信息，但私钥文件缺失；拒绝重新生成，原配置已保留"
            );
            (KeyPair::new(), true)
        }
        Err(error) => return Err(error).context("无法检查 Biscuit 私钥文件"),
    };
    if let Some(public) = public {
        ensure!(
            key.public() == public,
            "私钥与配置 rpc.biscuit_public_key 不匹配；原密钥与配置已保留"
        );
    }
    rpc.insert("biscuit_public_key".into(), key.public().to_string().into());
    rpc.insert(
        "biscuit_private_key_path".into(),
        key_path.to_str().context("私钥路径必须是 UTF-8")?.into(),
    );
    let yaml = serde_yaml::to_string(&document).context("无法编码 LSP 配置")?;
    // Prepare the configuration before creating the key. Keep the original config permissions.
    let mut temp = tempfile::NamedTempFile::new_in(config.parent().unwrap())?;
    temp.as_file().set_permissions(metadata.permissions())?;
    temp.write_all(yaml.as_bytes())?;
    temp.as_file().sync_all()?;
    if new_key {
        let text = Zeroizing::new(format!("{}\n", key.private().to_prefixed_string()));
        store::create_private(&key_path, text.as_bytes())?;
    }
    temp.persist(&config)
        .map_err(|error| error.error)
        .with_context(|| format!("无法保存 {}；已有私钥保留，可重试初始化", config.display()))?;
    fs::File::open(config.parent().unwrap())?.sync_all()?;
    Ok(InitializedLsp {
        config,
        private_key: key_path,
    })
}

fn add_to_list(value: Option<&mut Value>, additions: &[&str], field: &str) -> Result<()> {
    let list = value
        .and_then(Value::as_sequence_mut)
        .with_context(|| format!("{field} 必须是列表；请使用 offckb 生成的配置"))?;
    ensure!(
        list.iter().all(|item| item.as_str().is_some()),
        "{field} 必须是字符串列表"
    );
    for addition in additions {
        if !list
            .iter()
            .any(|item| item.as_str().unwrap().eq_ignore_ascii_case(addition))
        {
            list.push((*addition).into());
        }
    }
    Ok(())
}

fn optional_string<'a>(mapping: &'a Mapping, field: &str) -> Result<Option<&'a str>> {
    match mapping.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(text)) => Ok((!text.trim().is_empty()).then_some(text.trim())),
        Some(_) => bail!("rpc.{field} 必须是字符串；原配置已保留"),
    }
}

#[derive(Args)]
#[command(
    after_help = "默认离线读取 offckb Node3 的实际配置，无需启动 LSP。\n相对路径均相对于当前工作目录。私钥必须是仅当前用户可访问的普通文件。\n目标已存在（含符号链接）时拒绝覆盖。权限不包含 write(\"node\")。"
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

pub fn create_token(options: &CreateToken) -> Result<CreatedToken> {
    let config = crate::rpc::config_path(options.config.as_deref())?;
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
    let root = read_keypair(&key_path)?;
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

fn read_keypair(key_path: &Path) -> Result<KeyPair> {
    // Open without following a symlink and validate the opened file, avoiding a check/read race.
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(key_path)
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
    Ok(KeyPair::from(&private))
}

#[cfg(test)]
#[path = "test_admin.rs"]
mod tests;

/// Validate the initialized LSP and public wallets before any user metadata is written.
pub async fn init_accounts(options: &InitAccounts) -> Result<()> {
    let config = crate::rpc::Config::load(options.config.as_deref())?;
    let path = crate::rpc::config_path(options.config.as_deref())?;
    let document: Value = serde_yaml::from_slice(&fs::read(path)?)?;
    ensure!(
        document["services"]
            .as_sequence()
            .is_some_and(|services| services.iter().any(|s| s.as_str() == Some("lsp"))),
        "先执行 admin init-lsp 初始化 LSP 配置"
    );
    let bindings = (6..=9)
        .map(crate::wallet::public_binding)
        .collect::<Result<Vec<_>>>()?;
    crate::accounts::init_users(&config, bindings).await
}

#[derive(Args)]
pub struct InitAccounts {
    /// 已初始化的 Node3 配置；仅离线读取，不修改 YAML 或 LSP 密钥
    #[arg(long, value_name = "PATH")]
    pub config: Option<PathBuf>,
}
