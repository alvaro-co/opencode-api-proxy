use std::collections::HashSet;
use std::time::Duration;

const FETCH_TIMEOUT: Duration = Duration::from_secs(10);
const FETCH_BODY_LIMIT: usize = 1024 * 1024;
const SCRAPE_CONCURRENCY: usize = 16;

pub const DEFAULT_HTTP_SOURCES: &[&str] = &[
    "https://api.proxyscrape.com/v2/?request=displayproxies&protocol=http",
    "https://api.proxyscrape.com/v3/free-proxy-list/get?request=getproxies&protocol=http",
    "https://api.proxyscrape.com/v3/free-proxy-list/get?request=getproxies&protocol=https",
    "https://api.proxyscrape.com/v4/free-proxy-list/get?request=display_proxies&proxy_format=protocolipport&format=text",
    "https://openproxylist.xyz/http.txt",
    "https://openproxylist.xyz/https.txt",
    "https://raw.githubusercontent.com/Anonym0usWork1221/Free-Proxies/refs/heads/main/proxy_files/http_proxies.txt",
    "https://raw.githubusercontent.com/ObcbO/getproxy/refs/heads/master/file/http.txt",
    "https://raw.githubusercontent.com/ObcbO/getproxy/refs/heads/master/file/https.txt",
    "https://raw.githubusercontent.com/TheSpeedX/PROXY-List/refs/heads/master/http.txt",
    "https://raw.githubusercontent.com/TuanMinPay/live-proxy/refs/heads/master/http.txt",
    "https://raw.githubusercontent.com/databay-labs/free-proxy-list/refs/heads/master/http.txt",
    "https://raw.githubusercontent.com/dinoz0rg/proxy-list/refs/heads/main/checked_proxies/http.txt",
    "https://raw.githubusercontent.com/dpangestuw/Free-PROXY/refs/heads/main/http_proxies.txt",
    "https://raw.githubusercontent.com/ebrasha/abdal-proxy-hub/refs/heads/main/http-proxy-list-by-EbraSha.txt",
    "https://raw.githubusercontent.com/ebrasha/abdal-proxy-hub/refs/heads/main/https-proxy-list-by-EbraSha.txt",
    "https://raw.githubusercontent.com/hproxy-com/free-proxy-list/refs/heads/main/http.txt",
    "https://raw.githubusercontent.com/hproxy-com/free-proxy-list/refs/heads/main/https.txt",
    "https://raw.githubusercontent.com/iplocate/free-proxy-list/refs/heads/main/protocols/http.txt",
    "https://raw.githubusercontent.com/proxifly/free-proxy-list/refs/heads/main/proxies/protocols/http/data.txt",
    "https://raw.githubusercontent.com/proxifly/free-proxy-list/refs/heads/main/proxies/protocols/https/data.txt",
    "https://raw.githubusercontent.com/roosterkid/openproxylist/refs/heads/main/HTTPS_RAW.txt",
    "https://raw.githubusercontent.com/sunny9577/proxy-scraper/refs/heads/master/generated/http_proxies.txt",
    "https://raw.githubusercontent.com/zloi-user/hideip.me/refs/heads/main/https.txt",
    "https://raw.githubusercontent.com/zloi-user/hideip.me/refs/heads/main/connect.txt",
    "https://spys.me/proxy.txt",
    "https://raw.githubusercontent.com/monosans/proxy-list/main/proxies/http.txt",
    "https://raw.githubusercontent.com/clarketm/proxy-list/master/proxy-list-raw.txt",
    "https://raw.githubusercontent.com/proxy4parsing/proxy-list/main/http.txt",
    "https://proxylist.geonode.com/api/proxy-list?limit=500&page=1&sort_by=lastChecked&sort_type=desc",
    "https://cdn.jsdelivr.net/gh/proxyscrape/free-proxy-list@main/proxies/protocols/http/data.txt",
    "https://cdn.jsdelivr.net/gh/proxyscrape/free-proxy-list@main/proxies/protocols/https/data.txt",
    "https://cdn.jsdelivr.net/gh/proxyscrape/free-proxy-list@main/proxies/all/data.txt",
    "https://api.proxyscrape.com/v4/free-proxy-list/get?request=display_proxies&protocol=http&proxy_format=protocolipport&format=text&anonymity=elite&timeout=5000",
    "https://api.proxyscrape.com/v4/free-proxy-list/get?request=display_proxies&protocol=http&proxy_format=protocolipport&format=text&country=us&anonymity=elite",
    "https://raw.githubusercontent.com/iplocate/free-proxy-list/main/all-proxies.txt",
    "https://raw.githubusercontent.com/vmheaven/VMHeaven.io-Free-Proxy-List/main/http.txt",
    "https://raw.githubusercontent.com/vmheaven/VMHeaven.io-Free-Proxy-List/main/https.txt",
    "https://raw.githubusercontent.com/r00tee/Proxy-List/main/Https.txt",
    "https://free-proxy-list.net/",
    "https://www.sslproxies.org/",
    "https://www.us-proxy.org/",
    "https://socks-proxy.net/",
];

pub const DEFAULT_SOCKS5_SOURCES: &[&str] = &[
    "https://api.proxyscrape.com/v2/?request=displayproxies&protocol=socks5",
    "https://api.proxyscrape.com/v3/free-proxy-list/get?request=getproxies&protocol=socks5",
    "https://openproxylist.xyz/socks5.txt",
    "https://raw.githubusercontent.com/Anonym0usWork1221/Free-Proxies/refs/heads/main/proxy_files/socks5_proxies.txt",
    "https://raw.githubusercontent.com/ObcbO/getproxy/refs/heads/master/file/socks5.txt",
    "https://raw.githubusercontent.com/TheSpeedX/PROXY-List/refs/heads/master/socks5.txt",
    "https://raw.githubusercontent.com/TuanMinPay/live-proxy/refs/heads/master/socks5.txt",
    "https://raw.githubusercontent.com/databay-labs/free-proxy-list/refs/heads/master/socks5.txt",
    "https://raw.githubusercontent.com/dinoz0rg/proxy-list/refs/heads/main/checked_proxies/socks5.txt",
    "https://raw.githubusercontent.com/dpangestuw/Free-PROXY/refs/heads/main/socks5_proxies.txt",
    "https://raw.githubusercontent.com/ebrasha/abdal-proxy-hub/refs/heads/main/socks5-proxy-list-by-EbraSha.txt",
    "https://raw.githubusercontent.com/hookzof/socks5_list/refs/heads/master/proxy.txt",
    "https://raw.githubusercontent.com/hproxy-com/free-proxy-list/refs/heads/main/socks5.txt",
    "https://raw.githubusercontent.com/iplocate/free-proxy-list/refs/heads/main/protocols/socks5.txt",
    "https://raw.githubusercontent.com/proxifly/free-proxy-list/refs/heads/main/proxies/protocols/socks5/data.txt",
    "https://raw.githubusercontent.com/roosterkid/openproxylist/refs/heads/main/SOCKS5_RAW.txt",
    "https://raw.githubusercontent.com/sunny9577/proxy-scraper/refs/heads/master/generated/socks5_proxies.txt",
    "https://raw.githubusercontent.com/zloi-user/hideip.me/refs/heads/main/socks5.txt",
    "https://raw.githubusercontent.com/monosans/proxy-list/main/proxies/socks5.txt",
    "https://cdn.jsdelivr.net/gh/proxyscrape/free-proxy-list@main/proxies/protocols/socks5/data.txt",
    "https://api.proxyscrape.com/v4/free-proxy-list/get?request=display_proxies&protocol=socks5&proxy_format=protocolipport&format=text&anonymity=elite&timeout=8000",
    "https://raw.githubusercontent.com/vmheaven/VMHeaven.io-Free-Proxy-List/main/socks5.txt",
    "https://raw.githubusercontent.com/r00tee/Proxy-List/main/Socks5.txt",
    "https://raw.githubusercontent.com/iplocate/free-proxy-list/main/protocols/socks5.txt",
];

pub fn checkerproxy_archive_urls(days: u32) -> Vec<String> {
    fn days_to_ymd(days_since_epoch: i64) -> (i32, u32, u32) {
        let z = days_since_epoch + 719468;
        let era = if z >= 0 { z } else { z - 146096 } / 146097;
        let doe = (z - era * 146097) as u64;
        let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
        let y = yoe as i64 + era * 400;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
        let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
        (if m <= 2 { y + 1 } else { y } as i32, m, d)
    }
    let now_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    let today = now_secs / 86400;
    (0..days)
        .map(|back| {
            let (y, m, d) = days_to_ymd(today - back as i64);
            format!("https://checkerproxy.net/v1/landing/archive/{y:04}-{m:02}-{d:02}")
        })
        .collect()
}

fn valid_endpoint(host: &str, port_str: &str) -> Option<String> {
    let port: u16 = port_str.parse().ok()?;
    if port == 0 {
        return None;
    }
    let host = host
        .trim()
        .trim_matches(|c| c == '[' || c == ']')
        .to_ascii_lowercase();
    if host.is_empty() || host.len() > 253 {
        return None;
    }

    let is_ipv4 = host.split('.').count() == 4
        && host.split('.').all(|o| {
            o.parse::<u8>().is_ok() || (o.len() <= 3 && o.bytes().all(|b| b.is_ascii_digit()))
        });
    let is_ipv6 = host.parse::<std::net::Ipv6Addr>().is_ok();
    let is_host = host == "localhost"
        || (host.contains('.')
            && host
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-'));
    if !is_ipv4 && !is_host && !is_ipv6 {
        return None;
    }

    if is_ipv6 {
        if host == "::1" || host == "::" {
            return None;
        }
        return Some(format!("[{host}]:{port}"));
    }

    if is_ipv4 {
        let o: Vec<&str> = host.split('.').collect();
        if o[0] == "10" || o[0] == "0" || o[0] == "127" {
            return None;
        }
        if o[0] == "192" && o[1] == "168" {
            return None;
        }
        if o[0] == "172"
            && let Ok(second) = o[1].parse::<u8>()
            && (16..=31).contains(&second)
        {
            return None;
        }
    }
    Some(format!("{host}:{port}"))
}

pub fn parse_endpoints(text: &str) -> Vec<String> {
    let mut out = Vec::new();

    for raw_tok in text.split(|c: char| {
        c.is_whitespace()
            || c == '"'
            || c == '\''
            || c == '<'
            || c == '>'
            || c == ','
            || c == ';'
            || c == '|'
            || c == '\n'
            || c == '\r'
            || c == '\t'
    }) {
        let tok = raw_tok
            .trim()
            .trim_matches(|c| c == '(' || c == ')' || c == '[' || c == ']' || c == '{' || c == '}');
        if tok.is_empty() || !tok.contains(':') {
            continue;
        }

        let no_scheme = match tok.find("://") {
            Some(i) => &tok[i + 3..],
            None => tok,
        };

        let hostport_cidr = match no_scheme.rfind('@') {
            Some(at) => &no_scheme[at + 1..],
            None => no_scheme,
        };

        let Some(colon) = hostport_cidr.rfind(':') else {
            continue;
        };
        let (host_cidr, port_raw) = (&hostport_cidr[..colon], &hostport_cidr[colon + 1..]);
        if host_cidr.contains(':') && !host_cidr.contains(']') {
            continue;
        }

        let port_str: String = port_raw
            .chars()
            .take_while(|c| c.is_ascii_digit())
            .collect();
        if port_str.is_empty() || port_str.len() > 5 {
            continue;
        }

        if let Some((host, cidr_s)) = host_cidr.split_once('/') {
            let Ok(prefix) = cidr_s.parse::<u8>() else {
                continue;
            };
            if !(24..=32).contains(&prefix) {
                continue;
            }
            let Ok(net_ip) = host.parse::<std::net::Ipv4Addr>() else {
                continue;
            };
            let host_bits = 32 - prefix as u32;
            let count = 1u32 << host_bits;
            if count > 256 {
                continue;
            }
            let base: u32 = net_ip.into();
            let mask = if host_bits == 32 {
                0
            } else {
                u32::MAX << host_bits
            };
            let net = base & mask;
            for i in 1..count.saturating_sub(1).max(1) {
                let ip = std::net::Ipv4Addr::from(net + i);
                if let Some(ep) = valid_endpoint(&ip.to_string(), &port_str) {
                    out.push(ep);
                    if out.len() > 2000 {
                        break;
                    }
                }
            }
            continue;
        }

        let host = host_cidr.split('/').next().unwrap_or(host_cidr);
        if let Some(ep) = valid_endpoint(host, &port_str) {
            out.push(ep);
        }
        if out.len() > 5000 {
            break;
        }
    }
    out
}

pub async fn scrape_endpoints(
    client: &crate::common::HttpsClient,
    extra_urls: &[String],
    archive_days: u32,
) -> Vec<String> {
    let mut urls: Vec<String> = Vec::new();
    urls.extend(DEFAULT_HTTP_SOURCES.iter().map(|s| s.to_string()));
    urls.extend(DEFAULT_SOCKS5_SOURCES.iter().map(|s| s.to_string()));
    urls.extend(checkerproxy_archive_urls(archive_days));
    urls.extend(extra_urls.iter().cloned());

    {
        let mut seen = HashSet::new();
        urls.retain(|u| seen.insert(u.clone()));
    }

    let sem = std::sync::Arc::new(tokio::sync::Semaphore::new(SCRAPE_CONCURRENCY));
    let mut join = tokio::task::JoinSet::new();
    for url in urls {
        let permit = sem.clone();
        let client = client.clone();
        join.spawn(async move {
            let _p = permit.acquire_owned().await;
            match crate::common::fetch_text_with(&client, &url, FETCH_TIMEOUT, FETCH_BODY_LIMIT)
                .await
            {
                Ok(text) => Some(parse_endpoints(&text)),
                Err(e) => {
                    eprintln!("scrape {url}: {e}");
                    None
                }
            }
        });
    }
    let mut endpoints: HashSet<String> = HashSet::new();
    while let Some(res) = join.join_next().await {
        if let Ok(Some(list)) = res {
            for ep in list {
                endpoints.insert(ep);
            }
        }
    }
    let mut v: Vec<String> = endpoints.into_iter().collect();

    for i in (1..v.len()).rev() {
        let j = fastrand::usize(..=i);
        v.swap(i, j);
    }
    v
}

pub fn expand_candidates(endpoints: &[String], cap: usize) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for ep in endpoints {
        for url in [format!("http://{ep}"), format!("socks5://{ep}")] {
            if seen.insert(url.clone()) {
                out.push(url);
                if out.len() >= cap {
                    return out;
                }
            }
        }
    }
    out
}
