use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use bytes::Bytes;
use clap::Parser;
use hickory_resolver::{
    TokioResolver, config::ResolverConfig, name_server::TokioConnectionProvider,
};
use http_body_util::BodyExt;
use hyper::{
    Request, StatusCode,
    client::conn::http1::handshake,
    header::{AUTHORIZATION, CONTENT_TYPE},
};
use hyper_util::rt::TokioIo;
use serde::Serialize;
use tabled::{Table, Tabled, settings::Style};
use tokio::{
    io::{AsyncRead, AsyncWrite},
    net::TcpStream,
};
use tokio_rustls::{
    TlsConnector,
    rustls::{ClientConfig, RootCertStore, pki_types::ServerName},
};
use url::Url;

mod provider;
use provider::{ContentEvent, Provider, SseEvent, next_sse_event};

// ── CLI ────────────────────────────────────────────────────────────────────

#[derive(Parser, Debug)]
#[command(name = "llm-ping", version, about = "LLM API latency diagnostic")]
struct Args {
    /// Provider type: ollama, openai, anthropic, gemini
    #[arg(short, long, default_value = "ollama")]
    r#type: String,

    /// API endpoint URL (default from --type)
    #[arg(short, long)]
    url: Option<String>,

    /// Model name (default from --type)
    #[arg(short, long)]
    model: Option<String>,

    /// Prompt text
    #[arg(short, long, default_value = "Say hello in one short sentence.")]
    prompt: String,

    /// Number of requests
    #[arg(short, long, default_value_t = 1)]
    count: u32,

    /// Warmup requests (not counted in stats)
    #[arg(long, default_value_t = 0)]
    warm: u32,

    /// API key (default: $OPENAI_API_KEY or $ANTHROPIC_API_KEY)
    #[arg(short = 'k', long)]
    api_key: Option<String>,

    /// Non-streaming mode
    #[arg(long)]
    no_stream: bool,

    /// Flush DNS cache between requests
    #[arg(long)]
    flush_dns: bool,

    /// JSON output
    #[arg(long)]
    json: bool,

    #[allow(dead_code)]
    /// Request timeout in seconds
    #[arg(long, default_value_t = 60)]
    timeout: u64,
}

// ── Per-request timings ─────────────────────────────────────────────────────

#[derive(Clone, Copy, Default)]
struct Phases {
    dns: Option<Duration>,
    tcp: Option<Duration>,
    tls: Option<Duration>,
    http_first_byte: Option<Duration>,
    ttft: Option<Duration>,
    generation: Option<Duration>,
    total: Duration,
}

#[derive(Clone, Serialize)]
struct ProbeResult {
    n: u32,
    phases: Phases,
    chars: usize,
    tokens: usize,
    error: Option<String>,
}

impl Serialize for Phases {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let mut st = s.serialize_struct("Phases", 7)?;
        st.serialize_field("dns_ms", &self.dns.map(|d| d.as_secs_f64() * 1000.0))?;
        st.serialize_field("tcp_ms", &self.tcp.map(|d| d.as_secs_f64() * 1000.0))?;
        st.serialize_field("tls_ms", &self.tls.map(|d| d.as_secs_f64() * 1000.0))?;
        st.serialize_field(
            "http_first_byte_ms",
            &self.http_first_byte.map(|d| d.as_secs_f64() * 1000.0),
        )?;
        st.serialize_field("ttft_ms", &self.ttft.map(|d| d.as_secs_f64() * 1000.0))?;
        st.serialize_field(
            "generation_ms",
            &self.generation.map(|d| d.as_secs_f64() * 1000.0),
        )?;
        st.serialize_field("total_ms", &(self.total.as_secs_f64() * 1000.0))?;
        st.end()
    }
}

// ── Connection + timing ─────────────────────────────────────────────────────

trait IoBox: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> IoBox for T {}

#[derive(Clone)]
struct ConnTiming {
    dns: Duration,
    tcp: Duration,
    tls: Duration,
}

async fn dial(
    url: &Url,
    dns_resolver: &TokioResolver,
) -> Result<(ConnTiming, TokioIo<Box<dyn IoBox>>), String> {
    let host = url.host_str().ok_or("no host in URL")?;
    let port = url.port_or_known_default().unwrap_or(80);

    let t_dns_start = Instant::now();
    let ips = dns_resolver
        .lookup_ip(host)
        .await
        .map_err(|e| format!("DNS lookup failed: {e}"))?
        .into_iter()
        .collect::<Vec<_>>();
    let ip = *ips.first().ok_or("no DNS records")?;
    let dns = t_dns_start.elapsed();

    let t_tcp_start = Instant::now();
    let tcp = TcpStream::connect((ip, port))
        .await
        .map_err(|e| format!("TCP connect failed: {e}"))?;
    let tcp_time = t_tcp_start.elapsed();
    let _ = tcp.set_nodelay(true);
    let t_tls_start = Instant::now();
    let (tls_time, io) = if url.scheme() == "https" {
        let root_store = RootCertStore::from_iter(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        let config = ClientConfig::builder()
            .with_root_certificates(root_store)
            .with_no_client_auth();
        let connector = TlsConnector::from(Arc::new(config));
        let name = ServerName::try_from(host)
            .map_err(|e| format!("invalid hostname: {e}"))?
            .to_owned();
        let tls = connector
            .connect(name, tcp)
            .await
            .map_err(|e| format!("TLS handshake failed: {e}"))?;
        (
            Some(t_tls_start.elapsed()),
            TokioIo::new(Box::new(tls) as Box<dyn IoBox>),
        )
    } else {
        (None, TokioIo::new(Box::new(tcp) as Box<dyn IoBox>))
    };

    Ok((
        ConnTiming {
            dns,
            tcp: tcp_time,
            tls: tls_time.unwrap_or_default(),
        },
        io,
    ))
}

// ── Single probe ────────────────────────────────────────────────────────────

async fn probe_once(
    args: &Args,
    dns_resolver: &TokioResolver,
    provider: &dyn Provider,
    url: &Url,
    model: &str,
    n: u32,
) -> ProbeResult {
    let t_start = Instant::now();

    let (conn_timing, io) = match dial(url, dns_resolver).await {
        Ok(v) => v,
        Err(e) => {
            return ProbeResult {
                n,
                phases: Phases::default(),
                chars: 0,
                tokens: 0,
                error: Some(e),
            };
        }
    };

    let (mut tx, conn) = match handshake(io).await {
        Ok(v) => v,
        Err(e) => {
            return ProbeResult {
                n,
                phases: Phases::default(),
                chars: 0,
                tokens: 0,
                error: Some(format!("HTTP handshake failed: {e}")),
            };
        }
    };
    tokio::spawn(conn);

    let body = provider.build_body(model, &args.prompt, !args.no_stream);
    let req = Request::post(url.as_str())
        .header(CONTENT_TYPE, "application/json")
        .header("Accept", "text/event-stream");

    let req = if let Some(ref key) = args.api_key {
        req.header(AUTHORIZATION, format!("Bearer {key}"))
    } else {
        req
    };

    let host = url.host_str().unwrap_or("localhost");
    let port = url.port_or_known_default().unwrap_or(80);
    let req = req.header("Host", format!("{host}:{port}"));

    let req = req
        .body(http_body_util::Full::new(Bytes::from(body)))
        .unwrap();

    let t_req_sent = Instant::now();
    let resp = match tx.send_request(req).await {
        Ok(r) => r,
        Err(e) => {
            return ProbeResult {
                n,
                phases: Phases::default(),
                chars: 0,
                tokens: 0,
                error: Some(format!("request failed: {e}")),
            };
        }
    };

    let t_resp_headers = Instant::now();
    let http_first_byte = t_resp_headers.duration_since(t_req_sent);

    if resp.status() != StatusCode::OK {
        let (_parts, body) = resp.into_parts();
        let body_bytes = body
            .collect()
            .await
            .map(|b| b.to_bytes())
            .unwrap_or_default();
        let err_body = String::from_utf8_lossy(&body_bytes).to_string();
        return ProbeResult {
            n,
            phases: Phases::default(),
            chars: 0,
            tokens: 0,
            error: Some(format!("HTTP {}: {err_body}", _parts.status)),
        };
    }

    // Read streaming body, parse via provider
    let mut body = resp.into_body();
    let mut buf = Vec::new();
    let mut first_token = true;
    let mut chars: usize = 0;
    let mut tokens: usize = 0;
    let mut t_first_token: Option<Instant> = None;

    loop {
        let chunk = match body.frame().await {
            Some(Ok(frame)) => frame,
            Some(Err(e)) => {
                return ProbeResult {
                    n,
                    phases: Phases::default(),
                    chars: 0,
                    tokens: 0,
                    error: Some(format!("stream error: {e}")),
                };
            }
            None => break,
        };

        if let Some(data) = chunk.data_ref() {
            buf.extend_from_slice(data);
            loop {
                let newline = buf.iter().position(|&b| b == b'\n');
                match newline {
                    Some(pos) => {
                        let line_bytes = buf[..pos].to_vec();
                        buf = buf[pos + 1..].to_vec();
                        let line = String::from_utf8_lossy(&line_bytes);

                        if let SseEvent::Data(data) = next_sse_event(&line) {
                            match provider.parse_chunk(data) {
                                ContentEvent::Token(content) => {
                                    if first_token {
                                        t_first_token = Some(Instant::now());
                                        first_token = false;
                                    }
                                    chars += content.len();
                                    tokens += content.len() / 4 + 1;
                                }
                                ContentEvent::Done(server_tokens) => {
                                    if let Some(tok) = server_tokens {
                                        tokens = tok;
                                    }
                                }
                                ContentEvent::None => {}
                            }
                        }
                    }
                    None => break,
                }
            }
        }
    }

    // Flush remaining buffer (non-streaming response without trailing \n)
    if !buf.is_empty() {
        let line = String::from_utf8_lossy(&buf);
        if let SseEvent::Data(data) = next_sse_event(&line) {
            match provider.parse_chunk(data) {
                ContentEvent::Token(content) => {
                    if first_token {
                        t_first_token = Some(Instant::now());
                    }
                    chars += content.len();
                    tokens += content.len() / 4 + 1;
                }
                ContentEvent::Done(server_tokens) => {
                    if let Some(tok) = server_tokens {
                        tokens = tok;
                    }
                }
                ContentEvent::None => {}
            }
        }
    }

    let t_end = Instant::now();

    let t_first = t_first_token.unwrap_or(t_end);
    let generation = if t_first > t_req_sent {
        Some(t_end.duration_since(t_first))
    } else {
        None
    };

    let phases = Phases {
        dns: Some(conn_timing.dns),
        tcp: Some(conn_timing.tcp),
        tls: Some(conn_timing.tls),
        http_first_byte: Some(http_first_byte),
        ttft: if t_first > t_req_sent {
            Some(t_first.duration_since(t_req_sent))
        } else {
            None
        },
        generation,
        total: t_end.duration_since(t_start),
    };

    ProbeResult {
        n,
        phases,
        chars,
        tokens,
        error: None,
    }
}

// ── Stats + display ─────────────────────────────────────────────────────────

#[derive(Tabled)]
struct Row {
    req: String,
    dns: String,
    tcp: String,
    tls: String,
    http_fb: String,
    ttft: String,
    gen_dur: String,
    tok_s: String,
    total: String,
    chars: usize,
}

fn fmt_dur(d: Option<Duration>) -> String {
    match d {
        Some(d) if d.as_micros() < 1000 => format!("{}µs", d.as_micros()),
        Some(d) => format!("{:.1}ms", d.as_secs_f64() * 1000.0),
        None => "-".to_string(),
    }
}

fn fmt_tput(tokens: usize, dur: Option<Duration>) -> String {
    match dur {
        Some(d) if d.as_secs_f64() > 0.0 => format!("{:.1}", tokens as f64 / d.as_secs_f64()),
        _ => "-".to_string(),
    }
}

fn fmt_row(r: &ProbeResult) -> Row {
    let p = &r.phases;
    Row {
        req: format!("#{}", r.n),
        dns: fmt_dur(p.dns),
        tcp: fmt_dur(p.tcp),
        tls: fmt_dur(p.tls),
        http_fb: fmt_dur(p.http_first_byte),
        ttft: fmt_dur(p.ttft),
        gen_dur: fmt_dur(p.generation),
        tok_s: fmt_tput(r.tokens, p.generation),
        total: fmt_dur(Some(p.total)),
        chars: r.chars,
    }
}

// ── Main ────────────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() {
    let args = Args::parse();

    simple_logger::SimpleLogger::new()
        .with_level(log::LevelFilter::Info)
        .env()
        .init()
        .ok();

    let args = Args {
        api_key: args.api_key.clone().or_else(|| {
            std::env::var("OPENAI_API_KEY")
                .ok()
                .or_else(|| std::env::var("ANTHROPIC_API_KEY").ok())
        }),
        ..args
    };

    let provider = provider::from_type(&args.r#type);
    let (default_url, default_model) = provider::defaults(&args.r#type);
    let url: Url = args
        .url
        .as_deref()
        .unwrap_or(default_url)
        .parse()
        .expect("invalid URL");
    let model: &str = args.model.as_deref().unwrap_or(default_model);

    let dns_resolver: TokioResolver = TokioResolver::builder_with_config(
        ResolverConfig::default(),
        TokioConnectionProvider::default(),
    )
    .build();

    for _ in 0..args.warm {
        let _ = probe_once(&args, &dns_resolver, &*provider, &url, model, 0).await;
    }

    let mut results = Vec::new();
    for n in 1..=args.count {
        let r = probe_once(&args, &dns_resolver, &*provider, &url, model, n).await;
        results.push(r);

        if args.flush_dns {
            // ponytail: not implemented — hickory caches internally
        }
    }

    if args.json {
        println!("{}", serde_json::to_string_pretty(&results).unwrap());
        return;
    }

    let rows: Vec<Row> = results.iter().map(fmt_row).collect();
    let mut table = Table::new(rows);
    table.with(Style::modern());
    println!("\nllm-ping — {} ({})", args.r#type, url);
    println!("model: {}, prompt: {} chars", model, args.prompt.len());
    if args.warm > 0 {
        println!("(warmup: {} requests not shown)", args.warm);
    }
    println!();
    println!("{table}");
    println!();

    let ok_results: Vec<_> = results.iter().filter(|r| r.error.is_none()).collect();
    if ok_results.is_empty() {
        println!("All requests failed.");
        for r in &results {
            if let Some(ref e) = r.error {
                println!("  #{}: {e}", r.n);
            }
        }
        return;
    }

    let count = ok_results.len();
    let avg_ttft = ok_results
        .iter()
        .filter_map(|r| r.phases.ttft)
        .sum::<Duration>()
        .as_secs_f64()
        / count as f64;
    let total_tokens: usize = ok_results.iter().map(|r| r.tokens).sum();
    let total_gen: f64 = ok_results
        .iter()
        .filter_map(|r| r.phases.generation)
        .sum::<Duration>()
        .as_secs_f64();

    let avg_tok_s = if total_gen > 0.0 {
        total_tokens as f64 / total_gen
    } else {
        0.0
    };

    println!(
        "avg TTFT: {:.1}ms  |  avg throughput: {:.1} tok/s  |  total tokens: {}",
        avg_ttft * 1000.0,
        avg_tok_s,
        total_tokens,
    );

    for r in &results {
        if let Some(ref e) = r.error {
            log::error!("  #{} error: {e}", r.n);
        }
    }
}
