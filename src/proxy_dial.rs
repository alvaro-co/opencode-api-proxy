use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use hyper::{Method, Request, Response, Uri};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use crate::common::Err;

const DIAL_TIMEOUT: Duration = Duration::from_secs(10);
const TCP_DIAL_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProxyScheme {
    Http,
    Socks5,
}

#[derive(Debug, Clone)]
pub struct ProxyParts {
    pub scheme: ProxyScheme,
    pub host: String,
    pub port: u16,
    pub username: Option<String>,
    pub password: Option<String>,
}

pub fn parse_proxy(proxy_url: &str) -> Result<ProxyParts, String> {
    let p = proxy_url.trim();
    if p.is_empty() {
        return Err("empty proxy".to_string());
    }

    let low = p.to_ascii_lowercase();
    if low.starts_with("socks4://") || low.starts_with("socks4a://") {
        return Err("socks4 skipped in v1 (http+socks5 only)".to_string());
    }

    let (scheme_str, rest) = match p.find("://") {
        Some(idx) => (Some(p[..idx].to_ascii_lowercase()), &p[idx + 3..]),
        None => (None, p),
    };
    let scheme = match scheme_str.as_deref() {
        None => ProxyScheme::Http,
        Some("http") | Some("https") => ProxyScheme::Http,
        Some("socks5") | Some("socks5h") | Some("socks") => ProxyScheme::Socks5,
        Some(other) => return Err(format!("unsupported proxy scheme: {other}")),
    };

    let (auth_part, hostport) = match rest.rfind('@') {
        Some(at) => (Some(&rest[..at]), &rest[at + 1..]),
        None => (None, rest),
    };
    let (mut username, mut password): (Option<String>, Option<String>) = (None, None);
    if let Some(a) = auth_part {
        if let Some((u, pw)) = a.split_once(':') {
            if !u.is_empty() {
                username = Some(u.to_string());
            }
            password = Some(pw.to_string());
        } else if !a.is_empty() {
            username = Some(a.to_string());
        }
    }

    if scheme_str.is_none() && auth_part.is_none() {
        let parts: Vec<&str> = hostport.split(':').collect();
        if parts.len() == 4 {
            let (ip, port_s, user, pass) = (parts[0], parts[1], parts[2], parts[3]);
            let port: u16 = port_s.parse().map_err(|_| format!("bad port in {p}"))?;
            return Ok(ProxyParts {
                scheme: ProxyScheme::Http,
                host: ip.to_string(),
                port,
                username: if user.is_empty() {
                    None
                } else {
                    Some(user.to_string())
                },
                password: Some(pass.to_string()),
            });
        }
    }

    let hostport = hostport.split('/').next().unwrap_or(hostport);
    let (host, port_s) = hostport
        .rfind(':')
        .map(|c| (&hostport[..c], &hostport[c + 1..]))
        .ok_or_else(|| format!("proxy missing host:port: {p}"))?;
    if host.is_empty() {
        return Err(format!("proxy missing host: {p}"));
    }

    let host = host.trim_matches(|c| c == '[' || c == ']').to_string();
    let port: u16 = port_s
        .parse()
        .map_err(|_| format!("bad proxy port in {p}"))?;
    if port == 0 {
        return Err(format!("bad proxy port in {p}"));
    }

    Ok(ProxyParts {
        scheme,
        host,
        port,
        username,
        password,
    })
}

pub fn is_socks5(proxy_url: &str) -> bool {
    matches!(parse_proxy(proxy_url), Ok(p) if p.scheme == ProxyScheme::Socks5)
}

fn target_from_uri(uri: &Uri) -> Result<(String, u16, bool), Err> {
    let host = uri
        .host()
        .ok_or_else(|| Box::from("request URI missing host") as Err)?
        .to_string();
    let is_https = uri
        .scheme_str()
        .is_some_and(|s| s.eq_ignore_ascii_case("https"));
    let port = uri.port_u16().unwrap_or(if is_https { 443 } else { 80 });
    Ok((host, port, is_https))
}

async fn tcp_connect(host: &str, port: u16) -> Result<TcpStream, Err> {
    let addr = if host.contains(':') && !host.starts_with('[') {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    };
    let stream = tokio::time::timeout(TCP_DIAL_TIMEOUT, TcpStream::connect(&addr))
        .await
        .map_err(|_| Box::from(format!("proxy tcp connect timed out: {addr}")) as Err)?
        .map_err(|e| Box::from(format!("proxy tcp connect {addr}: {e}")) as Err)?;
    let _ = stream.set_nodelay(true);
    Ok(stream)
}

async fn socks5_connect(
    mut stream: TcpStream,
    target_host: &str,
    target_port: u16,
    username: Option<&str>,
    password: Option<&str>,
) -> Result<TcpStream, Err> {
    let has_auth = username.is_some_and(|u| !u.is_empty());
    if has_auth {
        stream
            .write_all(&[0x05, 0x02, 0x00, 0x02])
            .await
            .map_err(|e| Box::new(e) as Err)?;
    } else {
        stream
            .write_all(&[0x05, 0x01, 0x00])
            .await
            .map_err(|e| Box::new(e) as Err)?;
    }
    let mut method_reply = [0u8; 2];
    tokio::time::timeout(DIAL_TIMEOUT, stream.read_exact(&mut method_reply))
        .await
        .map_err(|_| Box::from("socks5 greeting timed out") as Err)?
        .map_err(|e| Box::new(e) as Err)?;
    if method_reply[0] != 0x05 {
        return Err(format!("bad socks5 version: {}", method_reply[0]).into());
    }
    match method_reply[1] {
        0x00 => {}
        0x02 => {
            let user = username.unwrap_or("");
            let pass = password.unwrap_or("");
            let ub = user.as_bytes();
            let pb = pass.as_bytes();
            if ub.len() > 255 || pb.len() > 255 {
                return Err("socks5 credentials too long".into());
            }
            let mut req = Vec::with_capacity(3 + ub.len() + pb.len());
            req.push(0x01);
            req.push(ub.len() as u8);
            req.extend_from_slice(ub);
            req.push(pb.len() as u8);
            req.extend_from_slice(pb);
            stream
                .write_all(&req)
                .await
                .map_err(|e| Box::new(e) as Err)?;
            let mut auth_reply = [0u8; 2];
            tokio::time::timeout(DIAL_TIMEOUT, stream.read_exact(&mut auth_reply))
                .await
                .map_err(|_| Box::from("socks5 auth timed out") as Err)?
                .map_err(|e| Box::new(e) as Err)?;
            if auth_reply[1] != 0x00 {
                return Err("socks5 auth failed".into());
            }
        }
        0xFF => return Err("socks5: no acceptable auth method".into()),
        m => return Err(format!("socks5: unsupported method {m:#x}").into()),
    }

    let mut req = vec![0x05, 0x01, 0x00];
    if let Ok(ipv4) = target_host.parse::<std::net::Ipv4Addr>() {
        req.push(0x01);
        req.extend_from_slice(&ipv4.octets());
    } else if let Ok(ipv6) = target_host.parse::<std::net::Ipv6Addr>() {
        req.push(0x04);
        req.extend_from_slice(&ipv6.octets());
    } else {
        let hb = target_host.as_bytes();
        if hb.len() > 255 {
            return Err("socks5 target hostname too long".into());
        }
        req.push(0x03);
        req.push(hb.len() as u8);
        req.extend_from_slice(hb);
    }
    req.extend_from_slice(&target_port.to_be_bytes());
    stream
        .write_all(&req)
        .await
        .map_err(|e| Box::new(e) as Err)?;

    let mut hdr = [0u8; 4];
    tokio::time::timeout(DIAL_TIMEOUT, stream.read_exact(&mut hdr))
        .await
        .map_err(|_| Box::from("socks5 connect timed out") as Err)?
        .map_err(|e| Box::new(e) as Err)?;
    if hdr[0] != 0x05 {
        return Err("bad socks5 connect reply".into());
    }
    if hdr[1] != 0x00 {
        return Err(format!("socks5 connect failed: rep={:#x}", hdr[1]).into());
    }

    match hdr[3] {
        0x01 => {
            let mut b = [0u8; 4 + 2];
            stream
                .read_exact(&mut b)
                .await
                .map_err(|e| Box::new(e) as Err)?;
        }
        0x03 => {
            let mut l = [0u8; 1];
            stream
                .read_exact(&mut l)
                .await
                .map_err(|e| Box::new(e) as Err)?;
            let mut b = vec![0u8; l[0] as usize + 2];
            stream
                .read_exact(&mut b)
                .await
                .map_err(|e| Box::new(e) as Err)?;
        }
        0x04 => {
            let mut b = [0u8; 16 + 2];
            stream
                .read_exact(&mut b)
                .await
                .map_err(|e| Box::new(e) as Err)?;
        }
        a => return Err(format!("bad socks5 atyp: {a:#x}").into()),
    }
    Ok(stream)
}

fn tls_config() -> std::sync::Arc<rustls::ClientConfig> {
    use std::sync::OnceLock;
    static CFG: OnceLock<std::sync::Arc<rustls::ClientConfig>> = OnceLock::new();
    CFG.get_or_init(|| {
        let mut roots = rustls::RootCertStore::empty();
        roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        let mut cfg = rustls::ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth();
        cfg.alpn_protocols = vec![b"http/1.1".to_vec()];
        std::sync::Arc::new(cfg)
    })
    .clone()
}

async fn dial_socks5(
    parts: &ProxyParts,
    target_host: &str,
    target_port: u16,
) -> Result<TcpStream, Err> {
    let stream = tcp_connect(&parts.host, parts.port).await?;
    tokio::time::timeout(
        DIAL_TIMEOUT,
        socks5_connect(
            stream,
            target_host,
            target_port,
            parts.username.as_deref(),
            parts.password.as_deref(),
        ),
    )
    .await
    .map_err(|_| Box::from("socks5 handshake timed out") as Err)?
}

pub async fn request_via_socks5(
    proxy_url: &str,
    req: Request<http_body_util::combinators::BoxBody<Bytes, Err>>,
) -> Result<Response<Incoming>, Err> {
    let parts = parse_proxy(proxy_url).map_err(|e| Box::from(e) as Err)?;
    if parts.scheme != ProxyScheme::Socks5 {
        return Err("not a socks5 proxy".into());
    }
    let (orig_parts, orig_body) = req.into_parts();
    let orig_uri = orig_parts.uri.clone();
    let (target_host, target_port, is_https) = target_from_uri(&orig_uri)?;
    let body_bytes = tokio::time::timeout(DIAL_TIMEOUT, orig_body.collect())
        .await
        .map_err(|_| Box::from("timed out reading proxied request body") as Err)??
        .to_bytes();

    let stream = dial_socks5(&parts, &target_host, target_port).await?;

    let path = orig_uri
        .path_and_query()
        .map(|pq| pq.as_str().to_string())
        .unwrap_or_else(|| "/".to_string());
    let mut builder = Request::builder()
        .method(orig_parts.method.clone())
        .uri(path);
    for (k, v) in orig_parts.headers.iter() {
        if k == hyper::header::HOST || k == hyper::header::PROXY_AUTHORIZATION {
            continue;
        }
        builder = builder.header(k, v);
    }

    let host_val = if (is_https && target_port == 443) || (!is_https && target_port == 80) {
        target_host.clone()
    } else {
        format!("{target_host}:{target_port}")
    };
    builder = builder.header(hyper::header::HOST, host_val);
    let full_req = builder
        .body(Full::new(body_bytes))
        .map_err(|e| Box::new(e) as Err)?;

    let fut = async move {
        use hyper_util::rt::TokioIo;
        if is_https {
            use rustls::pki_types::ServerName;
            let name = ServerName::try_from(target_host.clone())
                .map_err(|_| Box::from("invalid TLS server name") as Err)?
                .to_owned();
            let tls = tokio_rustls::TlsConnector::from(tls_config());
            let tls_stream = tls
                .connect(name, stream)
                .await
                .map_err(|e| Box::from(format!("socks5 TLS: {e}")) as Err)?;
            let io = TokioIo::new(tls_stream);
            let (mut sender, conn) = hyper::client::conn::http1::handshake(io)
                .await
                .map_err(|e| Box::new(e) as Err)?;
            tokio::spawn(async move {
                let _ = conn.await;
            });
            let res = sender
                .send_request(full_req)
                .await
                .map_err(|e| Box::new(e) as Err)?;
            Ok(res)
        } else {
            let io = TokioIo::new(stream);
            let (mut sender, conn) = hyper::client::conn::http1::handshake(io)
                .await
                .map_err(|e| Box::new(e) as Err)?;
            tokio::spawn(async move {
                let _ = conn.await;
            });
            let res = sender
                .send_request(full_req)
                .await
                .map_err(|e| Box::new(e) as Err)?;
            Ok(res)
        }
    };
    tokio::time::timeout(DIAL_TIMEOUT, fut)
        .await
        .map_err(|_| Box::from("socks5 request timed out") as Err)?
}

pub async fn get_via_proxy(proxy_url: &str, url: &str) -> Result<(hyper::StatusCode, Bytes), Err> {
    let parts = parse_proxy(proxy_url).map_err(|e| Box::from(e) as Err)?;
    let req = Request::builder()
        .method(Method::GET)
        .uri(url)
        .header("User-Agent", crate::common::UA_HEADER.clone())
        .body(crate::common::full_body(Bytes::new()))
        .map_err(|e| Box::new(e) as Err)?;
    let res = match parts.scheme {
        ProxyScheme::Http => crate::common::build_proxy_request(proxy_url, req).await?,
        ProxyScheme::Socks5 => request_via_socks5(proxy_url, req).await?,
    };
    let status = res.status();
    let bytes = res
        .into_body()
        .collect()
        .await
        .map_err(|e| Box::new(e) as Err)?
        .to_bytes();
    Ok((status, bytes))
}
