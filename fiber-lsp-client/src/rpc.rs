use std::{
    env, fs,
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
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use zeroize::Zeroizing;

pub struct Connection {
    pub root: PathBuf,
    pub rpc_url: String,
    pub registration_token: Option<Zeroizing<String>>,
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

impl Connection {
    pub fn new(
        root: PathBuf,
        rpc_url: String,
        registration_token: Option<Zeroizing<String>>,
    ) -> Result<Self> {
        let url = reqwest::Url::parse(&rpc_url).context("LSP RPC URL 无效")?;
        anyhow::ensure!(
            matches!(url.scheme(), "http" | "https")
                && url.host_str().is_some()
                && url.username().is_empty()
                && url.password().is_none(),
            "LSP RPC URL 必须为不含凭证的 HTTP(S) 地址"
        );
        Ok(Self {
            root,
            rpc_url,
            registration_token,
        })
    }

    pub fn connect(
        options: &crate::ConnectionOptions,
        dir: &crate::store::AccountDir,
    ) -> Result<Self> {
        let saved: Option<Value> = if dir.is_initialized()? {
            Some(dir.read_json()?)
        } else {
            None
        };
        let rpc_url = options
            .rpc_url
            .clone()
            .or_else(|| saved.as_ref()?.get("rpc_url")?.as_str().map(str::to_owned))
            .context("首次接入必须指定 --rpc-url")?;
        Self::new(crate::accounts::root(), rpc_url, options.read_token()?)
    }

    /// Read the bootstrap credential only when registration or initial opening needs it.
    pub fn registration_token(&self) -> Result<Zeroizing<String>> {
        if let Some(token) = &self.registration_token {
            return Ok(token.clone());
        }
        let path = self.root.join("configs/keys/operator-token");
        let token = Zeroizing::new(fs::read_to_string(&path).with_context(|| {
            format!("无法读取默认管理凭证 {}；请先执行 admin create-token，或通过 --token / --token-file 指定", path.display())
        })?);
        anyhow::ensure!(
            !token.trim().is_empty(),
            "默认管理凭证 {} 为空",
            path.display()
        );
        Ok(Zeroizing::new(token.trim().to_owned()))
    }

    #[cfg(test)]
    pub fn account_dir(&self) -> PathBuf {
        self.root.join(".state/fiber-lsp-client/default")
    }
}

pub struct Rpc {
    service: &'static str,
    url: String,
    client: reqwest::Client,
    next_id: AtomicU64,
}

impl Rpc {
    pub fn new(url: &str) -> Result<Self> {
        Self::with_service(url, "lsp")
    }

    pub fn chain(url: &str) -> Result<Self> {
        Self::with_service(url, "ckb")
    }

    fn with_service(url: &str, service: &'static str) -> Result<Self> {
        Ok(Self {
            service,
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
        let mut span = crate::timings::Span::rpc(self.service, method);
        let result = self.call_inner(method, params, token).await;
        span.finish(result.is_ok());
        result
    }

    async fn call_inner<R: DeserializeOwned>(
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

#[derive(Debug)]
pub struct MissingRecord;
impl std::fmt::Display for MissingRecord {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("远端明确返回记录不存在")
    }
}
impl std::error::Error for MissingRecord {}

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
            // Classify only the exact missing-record responses of the current server.
            // Never treat a transport/auth/parse error as absence.
            let message = error.get("message").and_then(Value::as_str).unwrap_or("");
            if (method == "get_invoice" && message == "invoice not found")
                || (method == "get_payment"
                    && message.starts_with("InvalidParameter: Payment session not found: "))
            {
                return Err(MissingRecord.into());
            }
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
#[path = "test_rpc.rs"]
mod tests;
