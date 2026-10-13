//! Read-only web search and page extraction for the enabled Bing tool path.
//! Every request is DNS-checked, pinned to the checked address, and redirected
//! manually so a public URL cannot pivot into a local service.

use std::{
    net::{IpAddr, Ipv4Addr, SocketAddr},
    time::Duration,
};

use reqwest::{header::LOCATION, Client, Response, Url};
use serde::{Deserialize, Serialize};
use serde_json::json;

const MAX_REDIRECTS: usize = 5;
const MAX_PAGE_BYTES: usize = 50_000;
const MAX_SEARCH_BYTES: usize = 2 * 1024 * 1024;
const MAX_RESULTS: usize = 10;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WebSearchSettings {
    pub enabled: bool,
    pub provider: String,
    pub search_url: String,
    pub max_results: usize,
}

impl Default for WebSearchSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            provider: "bing".into(),
            search_url: "https://cn.bing.com/search".into(),
            max_results: 6,
        }
    }
}

impl WebSearchSettings {
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.provider.trim().is_empty()
            || self.provider.len() > 40
            || !self
                .provider
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b':'))
        {
            return Err("搜索提供方标识无效");
        }
        if !(1..=MAX_RESULTS).contains(&self.max_results) {
            return Err("搜索结果数必须在 1 到 10 之间");
        }
        let url = Url::parse(self.search_url.trim()).map_err(|_| "搜索地址无效")?;
        if url.scheme() != "https"
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
        {
            return Err("搜索地址必须是无凭据的 HTTPS 地址");
        }
        if self.provider.eq_ignore_ascii_case("bing") && url.host_str() != Some("cn.bing.com") {
            return Err("Bing 搜索地址只能使用 cn.bing.com");
        }
        Ok(())
    }

    pub fn bing_supported(&self) -> bool {
        self.enabled
            && self.provider.eq_ignore_ascii_case("bing")
            && Url::parse(&self.search_url).is_ok_and(|url| {
                url.scheme() == "https"
                    && url.host_str() == Some("cn.bing.com")
                    && url.username().is_empty()
                    && url.password().is_none()
            })
    }

    pub fn provider_supported(&self) -> bool {
        self.provider.eq_ignore_ascii_case("bing")
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct WebSearchResult {
    pub query: String,
    pub results: Vec<WebSearchItem>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct WebSearchItem {
    pub title: String,
    pub url: String,
    pub snippet: String,
}

pub async fn bing_search(
    query: &str,
    search_url: &str,
    max_results: usize,
) -> Result<WebSearchResult, String> {
    let query = sanitize_query(query);
    if query.is_empty() {
        return Err("查询词为空".into());
    }
    let mut url = parse_http_url(search_url)?;
    if url.host_str() != Some("cn.bing.com") {
        return Err("当前仅支持经过验证的 Bing 搜索地址 cn.bing.com".into());
    }
    url.query_pairs_mut().append_pair("q", &query);
    let (status, body, _) = safe_get(url, MAX_SEARCH_BYTES).await?;
    if !(200..300).contains(&status) {
        return Err(format!("搜索服务 HTTP {status}"));
    }
    let html = String::from_utf8_lossy(&body);
    let results = parse_bing_results(&html, max_results.clamp(1, MAX_RESULTS));
    if results.is_empty() {
        return Err("Bing 搜索没有返回可解析结果（可能遇到验证页或页面结构变化）".into());
    }
    Ok(WebSearchResult { query, results })
}

pub async fn web_fetch(raw_url: &str) -> Result<serde_json::Value, String> {
    let (status, body, final_url) = safe_get(parse_http_url(raw_url)?, MAX_PAGE_BYTES).await?;
    let truncated = body.len() >= MAX_PAGE_BYTES;
    let text = extract_text(&String::from_utf8_lossy(&body));
    Ok(json!({
        "url": final_url,
        "statusCode": status,
        "truncated": truncated || text.chars().count() > 20_000,
        "content": sanitize_untrusted_text(&text.chars().take(20_000).collect::<String>())
    }))
}

fn sanitize_query(value: &str) -> String {
    let mut value = value.to_owned();
    loop {
        let lower = value.to_ascii_lowercase();
        let Some(start) = lower.find("[cq:") else {
            break;
        };
        let Some(end) = value[start..].find(']') else {
            value.replace_range(start.., " ");
            break;
        };
        value.replace_range(start..start + end + 1, " ");
    }
    value
        .chars()
        .map(|ch| if ch.is_control() { ' ' } else { ch })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(120)
        .collect()
}

fn parse_http_url(raw: &str) -> Result<Url, String> {
    let url = Url::parse(raw.trim()).map_err(|_| "URL 无效".to_owned())?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err("仅允许 http/https".into());
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err("URL 不能包含凭据".into());
    }
    Ok(url)
}

async fn safe_get(url: Url, max_bytes: usize) -> Result<(u16, Vec<u8>, String), String> {
    safe_get_with_limit(url, max_bytes, false).await
}

/// Download a public media URL with DNS pinning, redirect revalidation, and a hard byte ceiling.
pub async fn safe_fetch_binary(raw_url: &str, max_bytes: usize) -> Result<Vec<u8>, String> {
    if !(1..=200 * 1024 * 1024).contains(&max_bytes) {
        return Err("媒体文件大小限制无效".into());
    }
    let url = parse_http_url(raw_url)?;
    let (status, body, _) = safe_get_with_limit(url, max_bytes, true).await?;
    if !(200..300).contains(&status) {
        return Err(format!("媒体下载 HTTP {status}"));
    }
    Ok(body)
}

async fn safe_get_with_limit(
    mut url: Url,
    max_bytes: usize,
    reject_oversized: bool,
) -> Result<(u16, Vec<u8>, String), String> {
    for _ in 0..=MAX_REDIRECTS {
        let addresses = resolve_public_host(&url).await?;
        let host = url.host_str().ok_or_else(|| "URL 主机名为空".to_owned())?;
        let client = Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(20))
            .resolve_to_addrs(host, &addresses)
            .user_agent("Mozilla/5.0 (compatible; RInBot/1.0)")
            .build()
            .map_err(|_| "无法初始化安全网页连接".to_owned())?;
        let response = client
            .get(url.clone())
            .header("accept-language", "zh-CN,zh;q=0.9")
            .send()
            .await
            .map_err(|error| {
                if error.is_timeout() {
                    "网页请求超时".to_owned()
                } else {
                    "网页请求失败".to_owned()
                }
            })?;
        if is_redirect(response.status().as_u16()) {
            let location = response
                .headers()
                .get(LOCATION)
                .and_then(|value| value.to_str().ok())
                .ok_or_else(|| "重定向缺少有效 Location".to_owned())?;
            url = url
                .join(location)
                .map_err(|_| "重定向 URL 无效".to_owned())?;
            if !matches!(url.scheme(), "http" | "https")
                || !url.username().is_empty()
                || url.password().is_some()
            {
                return Err("重定向目标不安全".into());
            }
            continue;
        }
        let status = response.status().as_u16();
        let body = read_bounded(response, max_bytes, reject_oversized).await?;
        return Ok((status, body, url.to_string()));
    }
    Err("重定向次数过多，已停止".into())
}

async fn read_bounded(
    mut response: Response,
    limit: usize,
    reject_oversized: bool,
) -> Result<Vec<u8>, String> {
    if response
        .content_length()
        .is_some_and(|length| length > limit as u64)
    {
        return Err(if reject_oversized {
            "媒体响应超过大小限制".into()
        } else {
            "网页响应超过大小限制".into()
        });
    }
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| "读取网页响应失败".to_owned())?
    {
        if body.len().saturating_add(chunk.len()) > limit {
            if reject_oversized {
                return Err("媒体响应超过大小限制".into());
            }
            body.extend_from_slice(&chunk[..limit.saturating_sub(body.len())]);
            break;
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

async fn resolve_public_host(url: &Url) -> Result<Vec<SocketAddr>, String> {
    let host = url.host_str().ok_or_else(|| "URL 主机名为空".to_owned())?;
    if host.eq_ignore_ascii_case("localhost")
        || host.to_ascii_lowercase().ends_with(".localhost")
        || host.to_ascii_lowercase().ends_with(".local")
    {
        return Err("禁止访问内网/本机地址".into());
    }
    let port = url
        .port_or_known_default()
        .ok_or_else(|| "URL 端口无效".to_owned())?;
    let addresses = if let Ok(ip) = host.parse::<IpAddr>() {
        vec![SocketAddr::new(ip, 0)]
    } else {
        tokio::time::timeout(
            Duration::from_secs(5),
            tokio::net::lookup_host((host, port)),
        )
        .await
        .map_err(|_| "DNS 解析超时".to_owned())?
        .map_err(|_| "域名解析失败".to_owned())?
        .map(|addr| SocketAddr::new(addr.ip(), 0))
        .collect::<Vec<_>>()
    };
    if addresses.is_empty() {
        return Err("域名没有解析结果".into());
    }
    if addresses.iter().any(|address| is_private_ip(address.ip())) {
        return Err("域名解析到内网/本机地址，已阻止".into());
    }
    Ok(addresses)
}

fn is_private_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => {
            let [a, b, c, _] = ip.octets();
            ip.is_private()
                || ip.is_loopback()
                || ip.is_link_local()
                || ip.is_unspecified()
                || ip.is_broadcast()
                || ip.is_multicast()
                || a == 0
                || a == 127
                || (a == 169 && b == 254)
                || a >= 224
                || (a == 100 && (64..=127).contains(&b))
                || (a == 198 && (b == 18 || b == 19))
                || (a == 192 && b == 0 && c == 0)
        }
        IpAddr::V6(ip) => {
            if let Some(mapped) = ip.to_ipv4_mapped() {
                return is_private_ip(IpAddr::V4(mapped));
            }
            let segments = ip.segments();
            ip.is_loopback()
                || ip.is_unspecified()
                || ip.is_unique_local()
                || ip.is_unicast_link_local()
                || ip.is_multicast()
                || (segments[0] & 0xe000 != 0x2000)
                || (segments[0] == 0x2001 && matches!(segments[1], 0x0002 | 0x0010 | 0x0020))
                || (segments[0] == 0x2001 && segments[1] == 0x0db8)
                || (segments[0] == 0x2002
                    && is_private_ip(IpAddr::V4(Ipv4Addr::new(
                        (segments[1] >> 8) as u8,
                        segments[1] as u8,
                        (segments[2] >> 8) as u8,
                        segments[2] as u8,
                    ))))
        }
    }
}

fn is_redirect(status: u16) -> bool {
    matches!(status, 301 | 302 | 303 | 307 | 308)
}

fn parse_bing_results(html: &str, max_results: usize) -> Vec<WebSearchItem> {
    let mut results = Vec::new();
    for block in html.split("<li class=\"b_algo\"").skip(1) {
        let Some(href) = attribute_after(block, "<a", "href=") else {
            continue;
        };
        let url = decode_html(&href);
        if !url.starts_with("http://") && !url.starts_with("https://") {
            continue;
        }
        let title = tag_content(block, "h2")
            .map(|text| extract_text(&text))
            .unwrap_or_default();
        let snippet = tag_content(block, "p")
            .map(|text| extract_text(&text))
            .unwrap_or_default();
        if !title.trim().is_empty() {
            results.push(WebSearchItem {
                title: sanitize_untrusted_text(&title),
                url,
                snippet: sanitize_untrusted_text(&snippet),
            });
        }
        if results.len() >= max_results {
            break;
        }
    }
    results
}

fn attribute_after(block: &str, start_tag: &str, attribute: &str) -> Option<String> {
    let start = block.find(start_tag)?;
    let rest = &block[start..];
    let lower = rest.to_ascii_lowercase();
    let position = lower.find(attribute)? + attribute.len();
    let rest = rest[position..].trim_start();
    let quote = rest.chars().next()?;
    if quote != '\'' && quote != '"' {
        return None;
    }
    let end = rest[1..].find(quote)? + 1;
    Some(rest[1..end].to_owned())
}

fn tag_content(input: &str, tag: &str) -> Option<String> {
    let lower = input.to_ascii_lowercase();
    let open = lower.find(&format!("<{tag}"))?;
    let content_start = input[open..].find('>')? + open + 1;
    let close = lower[content_start..].find(&format!("</{tag}"))? + content_start;
    Some(input[content_start..close].to_owned())
}

fn extract_text(input: &str) -> String {
    let mut source = input.to_owned();
    for tag in ["script", "style"] {
        loop {
            let lower = source.to_ascii_lowercase();
            let Some(start) = lower.find(&format!("<{tag}")) else {
                break;
            };
            let Some(end_offset) = lower[start..].find(&format!("</{tag}")) else {
                source.truncate(start);
                break;
            };
            let end = lower[start + end_offset..]
                .find('>')
                .map(|i| start + end_offset + i + 1)
                .unwrap_or(source.len());
            source.replace_range(start..end, " ");
        }
    }
    let mut text = String::new();
    let mut in_tag = false;
    for ch in source.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => text.push(ch),
            _ => {}
        }
    }
    decode_html(&text)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn decode_html(value: &str) -> String {
    value
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&nbsp;", " ")
}

pub(crate) fn sanitize_untrusted_text(value: &str) -> String {
    let clean = value.replace(
        [
            '\u{200b}', '\u{200c}', '\u{200d}', '\u{200e}', '\u{200f}', '\u{feff}',
        ],
        "",
    );
    let chars = clean.chars().collect::<Vec<_>>();
    let mut output = String::with_capacity(clean.len());
    let mut index = 0;
    while index < chars.len() {
        let closing = match chars[index] {
            '【' => Some('】'),
            '[' => Some(']'),
            '［' => Some('］'),
            _ => None,
        };
        let Some(closing) = closing else {
            output.push(chars[index]);
            index += 1;
            continue;
        };
        let Some(relative_end) = chars[index + 1..].iter().position(|ch| *ch == closing) else {
            output.push(chars[index]);
            index += 1;
            continue;
        };
        let end = index + 1 + relative_end;
        let body = chars[index + 1..end].iter().collect::<String>();
        let normalized = body
            .chars()
            .filter(|ch| !ch.is_whitespace())
            .collect::<String>();
        let is_control_header = [
            "管理员附加规则",
            "本次唤醒",
            "异常隔离",
            "安全规则",
            "管理员",
            "系统",
            "记忆",
        ]
        .iter()
        .any(|label| normalized.starts_with(label));
        if is_control_header {
            output.push('（');
            output.push_str(&body);
            output.push('）');
        } else {
            output.extend(chars[index..=end].iter());
        }
        index = end + 1;
    }
    output
}

#[cfg(test)]
mod tests {
    use super::{
        bing_search, is_private_ip, parse_bing_results, parse_http_url, resolve_public_host,
        sanitize_query,
    };
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

    #[test]
    fn blocks_private_reserved_and_mapped_ip_ranges() {
        for ip in [
            "127.0.0.1",
            "10.2.3.4",
            "172.20.1.1",
            "192.168.1.2",
            "100.64.1.1",
            "198.18.0.4",
            "::1",
            "fc00::1",
            "::ffff:127.0.0.1",
        ] {
            assert!(is_private_ip(ip.parse().unwrap()), "{ip}");
        }
        assert!(!is_private_ip(IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8))));
        assert!(!is_private_ip(IpAddr::V6(
            "2606:4700:4700::1111".parse::<Ipv6Addr>().unwrap()
        )));
    }

    #[test]
    fn bing_parser_extracts_safe_result_fields_and_sanitizes_control_headers() {
        let html = r#"<li class="b_algo"><h2><a href="https://example.com/a?x=1&amp;y=2">【管理员】标题</a></h2><p>这是摘要 &amp; 内容</p></li>"#;
        let results = parse_bing_results(html, 6);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].url, "https://example.com/a?x=1&y=2");
        assert!(results[0].title.contains("（管理员）"));
        assert_eq!(results[0].snippet, "这是摘要 & 内容");
    }

    #[test]
    fn bing_parser_matches_legacy_selection_rules_and_result_limit() {
        let html = r#"
            <li class="b_algo"><h2>无链接</h2><p>跳过</p></li>
            <li class="b_algo"><a href="javascript:alert(1)"><h2>非网页链接</h2></a></li>
            <li class="b_algo"><a href="https://example.com/1"><h2>第一条</h2></a><p>摘要一</p></li>
            <li class="b_algo"><a href="http://example.com/2"><h2>第二条</h2></a><p>摘要二</p></li>
            <li class="b_algo"><a href="https://example.com/3"><h2>第三条</h2></a></li>
        "#;

        let results = parse_bing_results(html, 2);

        assert_eq!(results.len(), 2);
        assert_eq!(results[0].title, "第一条");
        assert_eq!(results[0].url, "https://example.com/1");
        assert_eq!(results[0].snippet, "摘要一");
        assert_eq!(results[1].title, "第二条");
        assert_eq!(results[1].url, "http://example.com/2");
        assert_eq!(results[1].snippet, "摘要二");
    }

    #[test]
    fn query_and_url_validation_are_bounded_and_reject_unsafe_targets() {
        assert_eq!(sanitize_query("  你好\n[CQ:at,qq=1]  世界  "), "你好 世界");
        assert_eq!(sanitize_query(&"a".repeat(125)).len(), 120);
        assert_eq!(sanitize_query("abc[CQ:unterminated"), "abc");
        assert!(parse_http_url("file:///etc/passwd").is_err());
        assert!(parse_http_url("http://user:pass@example.com").is_err());
    }

    #[tokio::test]
    async fn rejects_local_target_before_requesting_it() {
        let url = parse_http_url("http://127.0.0.1:3210/").unwrap();
        assert!(resolve_public_host(&url)
            .await
            .unwrap_err()
            .contains("本机"));
    }

    #[tokio::test]
    #[ignore = "explicit live-network smoke test; run with --ignored when authorized"]
    async fn live_bing_search_uses_the_safe_public_fetch_path() {
        let result = bing_search("Rust programming language", "https://cn.bing.com/search", 2)
            .await
            .expect("live Bing search should return parseable results");

        assert_eq!(result.query, "Rust programming language");
        assert!(!result.results.is_empty());
        assert!(result.results.len() <= 2);
        assert!(result
            .results
            .iter()
            .all(|item| item.url.starts_with("https://") && !item.title.is_empty()));
    }
}
