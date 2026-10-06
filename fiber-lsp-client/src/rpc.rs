use std::{
    env, fs,
    net::SocketAddr,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};

use anyhow::{Context, Result, bail};
use fiber_json_types::{
    GetLspTenantRegistryNonceParams, GetLspTenantRegistryNonceResult, ListChannelsParams,
    ListChannelsResult, ListLspTenantsResult, LspServiceStatus, Pubkey, RegisterLspTenantParams,
    RegisterLspTenantResult,
};
use reqwest::header::{AUTHORIZATION, HeaderValue};
use serde::{Deserialize, de::DeserializeOwned};
use serde_json::{Value, json};
use zeroize::Zeroizing;

pub struct Config {
    pub root: PathBuf,
    pub rpc_url: String,
    pub rpc_auth_public_key: String,
}

pub fn config_path(path: Option<&Path>) -> Result<PathBuf> {
    let path = match path {
        Some(path) => path.to_path_buf(),
        None => node3_config(env::var_os("XDG_DATA_HOME"), env::var_os("HOME"))?,
    };
    Ok(std::path::absolute(path)?)
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

impl Config {
    pub fn load(path: Option<&Path>) -> Result<Self> {
        let path = config_path(path)?;
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .context("无法定位 fiber-lab 项目目录")?;
        Self::from_path(root, &path)
    }

    fn from_path(root: &Path, path: &Path) -> Result<Self> {
        #[derive(Deserialize)]
        struct ProjectConfig {
            rpc: RpcConfig,
        }
        #[derive(Deserialize)]
        struct RpcConfig {
            listening_addr: String,
            biscuit_public_key: String,
        }
        let bytes = fs::read(path)
            .with_context(|| format!("无法读取 {}；请检查路径或指定 --config", path.display()))?;
        let project: ProjectConfig = serde_yaml::from_slice(&bytes).map_err(|_| {
            anyhow::anyhow!(
                "{} 的 YAML 无效，或缺少 rpc.listening_addr / rpc.biscuit_public_key",
                path.display()
            )
        })?;
        let address: SocketAddr = project.rpc.listening_addr.parse().with_context(|| {
            format!(
                "{} 的 rpc.listening_addr 不是有效的 IP:端口",
                path.display()
            )
        })?;
        if project.rpc.biscuit_public_key.trim().is_empty() {
            bail!("{} 的 rpc.biscuit_public_key 为空", path.display());
        }
        Ok(Self {
            root: root.to_path_buf(),
            rpc_url: format!("http://{address}"),
            rpc_auth_public_key: project.rpc.biscuit_public_key,
        })
    }

    pub fn operator_token(&self) -> Result<Zeroizing<String>> {
        let path = self.root.join("configs/keys/operator-token");
        let text = Zeroizing::new(
            fs::read_to_string(&path)
                .with_context(|| format!("无法读取管理凭证 {}", path.display()))?,
        );
        if text.trim().is_empty() {
            bail!("管理凭证文件 {} 为空", path.display());
        }
        Ok(Zeroizing::new(text.trim().to_owned()))
    }

    #[cfg(test)]
    pub fn account_dir(&self) -> PathBuf {
        self.root.join(".state/fiber-lsp-client/default")
    }
}

pub struct Rpc {
    url: String,
    client: reqwest::Client,
    next_id: AtomicU64,
}

impl Rpc {
    pub fn new(url: &str) -> Result<Self> {
        Ok(Self {
            url: url.to_owned(),
            client: reqwest::Client::builder()
                .no_proxy()
                .redirect(reqwest::redirect::Policy::none())
                .timeout(Duration::from_secs(10))
                .build()
                .context("无法创建 HTTP 客户端")?,
            next_id: AtomicU64::new(1),
        })
    }

    pub async fn call<R: DeserializeOwned>(
        &self,
        method: &str,
        params: Value,
        token: &str,
    ) -> Result<R> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let mut auth = HeaderValue::from_str(&format!("Bearer {token}"))
            .map_err(|_| anyhow::anyhow!("{method} 的凭证无法用于 HTTP 请求头"))?;
        auth.set_sensitive(true);
        let response = self
            .client
            .post(&self.url)
            .headers(if token.is_empty() {
                reqwest::header::HeaderMap::new()
            } else {
                let mut headers = reqwest::header::HeaderMap::new();
                headers.insert(AUTHORIZATION, auth);
                headers
            })
            .json(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}))
            .send()
            .await
            .map_err(|error| {
                let reason = if error.is_timeout() {
                    "请求超时"
                } else if error.is_connect() {
                    "无法连接 RPC"
                } else {
                    "HTTP 传输失败"
                };
                anyhow::anyhow!("网络错误：{method} 到 {}：{reason}", self.url)
            })?;
        let status = response.status();
        if !status.is_success() {
            bail!("HTTP 错误：{method} 返回 {status}；请检查凭证和服务状态");
        }
        let body = response
            .bytes()
            .await
            .map_err(|_| anyhow::anyhow!("网络错误：读取 {method} 的响应失败或超时"))?;
        decode_response(&body, id, method)
    }

    pub async fn get_status(&self, token: &str) -> Result<LspServiceStatus> {
        self.call("lsp_get_status", json!([]), token).await
    }

    pub async fn list_tenants(&self, token: &str) -> Result<ListLspTenantsResult> {
        self.call("lsp_list_tenants", json!([]), token).await
    }

    pub async fn registry_nonce(
        &self,
        token: &str,
        root_signer_pubkey: Pubkey,
    ) -> Result<GetLspTenantRegistryNonceResult> {
        let params = GetLspTenantRegistryNonceParams { root_signer_pubkey };
        self.call("lsp_get_tenant_registry_nonce", json!([params]), token)
            .await
    }

    pub async fn register(
        &self,
        token: &str,
        params: RegisterLspTenantParams,
    ) -> Result<RegisterLspTenantResult> {
        self.call("lsp_register_tenant", json!([params]), token)
            .await
    }

    pub async fn list_channels(&self, token: &str) -> Result<ListChannelsResult> {
        let params = ListChannelsParams {
            pubkey: None,
            include_closed: None,
            only_pending: None,
        };
        self.call("list_channels", json!([params]), token).await
    }
    /// Pending records are a separate projection in the server, including failed
    /// actors which have already been removed. Merge both read-only projections.
    pub async fn list_all_channels(&self, token: &str) -> Result<ListChannelsResult> {
        let params = ListChannelsParams {
            pubkey: None,
            include_closed: Some(true),
            only_pending: None,
        };
        let mut all: ListChannelsResult =
            self.call("list_channels", json!([params]), token).await?;
        let params = ListChannelsParams {
            pubkey: None,
            include_closed: None,
            only_pending: Some(true),
        };
        let pending: ListChannelsResult =
            self.call("list_channels", json!([params]), token).await?;
        for channel in pending.channels {
            if !all
                .channels
                .iter()
                .any(|c| c.channel_id == channel.channel_id)
            {
                all.channels.push(channel);
            }
        }
        Ok(all)
    }
}

fn decode_response<R: DeserializeOwned>(body: &[u8], id: u64, method: &str) -> Result<R> {
    // Raw response bodies and serde errors can contain credentials.
    let value: Value = serde_json::from_slice(body)
        .map_err(|_| anyhow::anyhow!("数据解析错误：{method} 的响应不是有效 JSON"))?;
    if value.get("jsonrpc") != Some(&json!("2.0")) || value.get("id") != Some(&json!(id)) {
        bail!("数据解析错误：{method} 的 JSON-RPC 版本或请求编号不匹配");
    }
    match (value.get("result"), value.get("error")) {
        (None, Some(error)) => {
            let code = error
                .get("code")
                .and_then(Value::as_i64)
                .context("数据解析错误：JSON-RPC 错误缺少 code")?;
            // Server messages/data can echo tokens; report only method and code.
            bail!("JSON-RPC 错误：{method} 返回 code {code}；请检查凭证和服务状态");
        }
        (Some(result), None) => serde_json::from_value(result.clone())
            .map_err(|_| anyhow::anyhow!("数据解析错误：{method} 的结果字段不符合 RPC 类型")),
        _ => bail!("数据解析错误：{method} 必须返回 result 或 error 其中之一"),
    }
}

#[async_trait::async_trait]
impl fiber_lsp_sdk::TenantOpeningRpc for Rpc {
    async fn open_tenant_channel(
        &self,
        token: &str,
        request: fiber_json_types::OpenChannelWithExternalFundingParams,
    ) -> std::result::Result<fiber_json_types::OpenTenantChannelResult, fiber_lsp_sdk::SessionError>
    {
        self.call("open_tenant_channel", json!([request]), token)
            .await
            .map_err(|e| fiber_lsp_sdk::SessionError::Invalid(format!("{e:#}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_mismatched_response_and_redacts_server_errors() {
        assert!(
            decode_response::<Value>(br#"{"jsonrpc":"2.0","id":2,"result":{}}"#, 1, "test")
                .is_err()
        );
        let error = decode_response::<Value>(
            br#"{"jsonrpc":"2.0","id":1,"error":{"code":-1,"message":"secret-token","data":"secret-token"}}"#,
            1,
            "test",
        ).unwrap_err();
        assert!(error.to_string().contains("code -1"));
        assert!(!error.to_string().contains("secret-token"));
        let error = decode_response::<u64>(
            br#"{"jsonrpc":"2.0","id":1,"result":"secret-token"}"#,
            1,
            "test",
        )
        .unwrap_err();
        assert!(!error.to_string().contains("secret-token"));
    }

    #[test]
    fn missing_config_fields_name_the_file_and_field() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("config.yml");
        let error = Config::from_path(root.path(), &path)
            .err()
            .unwrap()
            .to_string();
        assert!(error.contains(path.to_str().unwrap()));
        assert!(error.contains("--config"));
        fs::write(&path, "rpc: {}\n").unwrap();
        let error = Config::from_path(root.path(), &path)
            .err()
            .unwrap()
            .to_string();
        assert!(error.contains(path.to_str().unwrap()));
        assert!(error.contains("rpc.listening_addr"));
    }

    #[test]
    fn default_config_needs_home_only_without_xdg() {
        assert_eq!(
            node3_config(Some("/data".into()), None).unwrap(),
            PathBuf::from("/data/offckb-nodejs/devnet/fiber/nodes/3/config.yml")
        );
        for xdg in [None, Some("".into())] {
            assert_eq!(
                node3_config(xdg, Some("/home/test".into())).unwrap(),
                PathBuf::from(
                    "/home/test/.local/share/offckb-nodejs/devnet/fiber/nodes/3/config.yml"
                )
            );
        }
        assert!(
            node3_config(None, None)
                .unwrap_err()
                .to_string()
                .contains("--config")
        );
    }

    #[test]
    fn external_config_keeps_account_and_token_in_project() {
        let root = tempfile::tempdir().unwrap();
        let node = tempfile::tempdir().unwrap();
        let path = node.path().join("config.yml");
        fs::write(
            &path,
            "services: [fiber, lsp]\nrpc:\n  listening_addr: 127.0.0.1:21716\n  biscuit_public_key: test-key\n  biscuit_private_key_path: /unused/key\n",
        )
        .unwrap();
        let config = Config::from_path(root.path(), &path).unwrap();
        assert_eq!(config.rpc_url, "http://127.0.0.1:21716");
        assert_eq!(config.rpc_auth_public_key, "test-key");
        assert_eq!(
            config.account_dir(),
            root.path().join(".state/fiber-lsp-client/default")
        );
        fs::create_dir_all(root.path().join("configs/keys")).unwrap();
        fs::write(
            root.path().join("configs/keys/operator-token"),
            "test-token",
        )
        .unwrap();
        assert_eq!(config.operator_token().unwrap().as_str(), "test-token");
    }
}
