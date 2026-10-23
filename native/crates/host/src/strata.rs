//! Strata's public health check as the Host reads it before saving a local Strata model (3.x `strata-health.ts`).

use std::time::Duration;

use serde::Deserialize;

use crate::locale::l;
use crate::network::Network;

/// What Strata reports about its loaded model; no credential, prompt or answer.
pub struct Health {
    pub base_url: String,
    pub model: String,
    pub context_window: u64,
}

#[derive(Deserialize)]
struct Reply {
    status: String,
    service: String,
    model: String,
    max_context: u64,
    loaded: bool,
    api_key: bool,
}

/// A Strata API root, which must be a local HTTP address with a port and the `/v1` path.
pub fn loopback_url(base: &str) -> Result<url::Url, String> {
    let invalid = || l("Strata 连接必须使用带端口的本机 HTTP 地址和 /v1 路径。", "A Strata connection must use a local HTTP address with a port and the /v1 path.");
    let mut url = url::Url::parse(base).map_err(|_| invalid())?;
    let local = matches!(url.host_str(), Some("127.0.0.1" | "localhost" | "[::1]"));
    let path = url.path().trim_end_matches('/') == "/v1";
    if url.scheme() != "http" || !local || !url.username().is_empty() || url.password().is_some() || url.query().is_some() || url.fragment().is_some() || url.port().is_none() || !path {
        return Err(invalid());
    }
    url.set_path("/v1");
    Ok(url)
}

/// Confirm this Host reaches a loaded Strata model that needs no key.
pub async fn inspect(network: &Network, base: &str) -> Result<Health, String> {
    let base = loopback_url(base)?;
    let health = base.join("/health").map_err(|error| error.to_string())?;
    let response = network.client(&health).await.get(health.clone()).timeout(Duration::from_secs(3)).send().await.map_err(|error| error.to_string())?;
    if !response.status().is_success() {
        let status = response.status().as_u16();
        return Err(l(&format!("Strata 健康检查失败（HTTP {status}）。"), &format!("The Strata status check failed (HTTP {status}).")));
    }
    let reply: Reply = response.json().await.map_err(|error| error.to_string())?;
    if reply.status != "ok" || reply.service != "strata" || reply.model.is_empty() || reply.max_context == 0 {
        return Err(l("Strata 返回的状态无法识别。", "Strata returned a status Rainy does not recognize."));
    }
    if !reply.loaded {
        return Err(l("Strata 模型尚未加载完成，请等待就绪后再连接。", "The Strata model has not finished loading. Wait until it is ready, then connect."));
    }
    if reply.api_key {
        return Err(l("此 Strata 服务需要 API 密钥，请在现有模型设置中配置凭据。", "This Strata server needs an API key. Add the key in your existing model settings."));
    }
    Ok(Health { base_url: base.to_string(), model: reply.model, context_window: reply.max_context })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_only_local_roots() {
        assert_eq!(loopback_url("http://127.0.0.1:8080/v1/").unwrap().as_str(), "http://127.0.0.1:8080/v1");
        assert!(loopback_url("http://127.0.0.1/v1").is_err());
        assert!(loopback_url("https://127.0.0.1:8080/v1").is_err());
        assert!(loopback_url("http://example.com:8080/v1").is_err());
        assert!(loopback_url("http://127.0.0.1:8080/v2").is_err());
    }
}
