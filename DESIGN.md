# llm-ping: 设计文档与踩坑分析

## 目标

像 `sshping` 测 SSH 延迟一样，逐层拆解 LLM API 调用的耗时：DNS、TCP、TLS、HTTP、TTFT、吞吐。

---

## 现状：现有工具缺口

| 工具 | 能做 | 不能做 |
|------|------|--------|
| **LLMPerf** (ray-project) | TTFT, inter-token latency, 并发吞吐 | 不拆网络层 (DNS/TCP/TLS)，依赖 Ray (重)，无实时显示，停更 |
| **OpenAI cookbook** 脚本 | 粗略总耗时 | 纯玩具，不拆层，已404 |
| **Anthropic cookbook** | Anthropic 专用 TTFT+吞吐 | 不通用 |
| **vLLM benchmark** | TTFT+吞吐 | 只测自家服务端 |

**结论：没有现成工具能同时满足：逐层拆时间、通用 OpenAI 兼容 API、轻量 CLI。**

---

## 指标分解

每次 streaming 请求，在客户端打时间戳：

```
T0 ──→ T1 ──→ T2 ──→ T3 ──→ T4 ──→ T5 ──→ T6 ──→ ... ──→ Tn
│      │      │      │      │      │      │               │
│      │      │      │      │      │      └─ 逐 chunk 到达 │
│      │      │      │      │      └──────── 首 chunk 到达   │
│      │      │      │      └─────────────── HTTP 响应头到达  │
│      │      │      └────────────────────── TLS 握手完成     │
│      │      └───────────────────────────── TCP 连接完成     │
│      └──────────────────────────────────── DNS 解析完成     │
└──────────────────────────────────────────── 请求发起         │
                                                                │
└──────────────────────────────────────────────────────── end ──┘
```

| 指标 | 计算 | 含义 |
|------|------|------|
| **DNS** | T1 - T0 | DNS 查询耗时（仅首次，复用后标记 reuse） |
| **TCP** | T2 - T1 | TCP 三次握手耗时 |
| **TLS** | T3 - T2 | TLS 握手耗时 |
| **HTTP 首字节** | T4 - T3 | HTTP 请求发出到响应头到达 |
| **TTFT** | T5 - T0 | 端到端首 token 延迟 |
| **服务端 TTFT** | T5 - T3 | 排除网络后的纯服务端首 token 延迟 |
| **平均吞吐** | total_tokens / (Tn - T5) | 生成阶段的 token/s |
| **端到端总耗时** | Tn - T0 | 从发起到流结束 |

---

## 逐阶段踩坑分析

### 1. DNS 阶段 (T0 → T1)

**如何测量**：使用 `hickory-resolver`（原 `trust-dns-resolver`）显式解析主机名，不依赖系统 libc 的 `getaddrinfo`。

**问题清单**：

| 问题 | 影响 | 对策 |
|------|------|------|
| **DNS 缓存** | 第二次请求 DNS 为 0ms，看起来像"网络极快" | 首次测真实 DNS，后续标记 `reuse`；支持 `--flush-dns` 强制重新解析 |
| **CNAME 链** | 一个域名可能经多次 CNAME 跳转才到 A 记录 | 只计到第一次成功解析的时间，不追完整链 |
| **IPv4 + IPv6 双栈** | Happy Eyeballs 算法可能同时发起两个查询 | 用 `hickory` 明确指定查询类型（默认 AAAA 优先），或并行查询取先返回的 |
| **EDNS / DNSSEC** | 查询可能因 DNSSEC 验证耗时更久 | 默认关闭 DNSSEC，加 `--dnssec` 选项 |
| **系统 hosts 文件** | `/etc/hosts` 中的条目会绕过 DNS | 直接用 hickory 不从 hosts 文件读；可以加选项支持本地覆盖 |
| **TLS 证书中的 LE 域名** | Let's Encrypt 等域名也是 DNS 的一部分 | 正常测量，不特殊处理 |

### 2. TCP 连接阶段 (T1 → T2)

**如何测量**：`tokio::net::TcpStream::connect(addr).await` 返回即代表 TCP 握手完成。

**问题清单**：

| 问题 | 影响 | 对策 |
|------|------|------|
| **TCP Fast Open (TFO)** | 如果服务端支持 TFO，握手会合并 SYN+数据，减少 1 RTT | `TcpStream` 默认关 TFO，不特殊启用；保持基准一致性 |
| **连接池复用** | HTTP 客户端（reqwest/hyper）复用 Keep-Alive 连接，后续请求 TCP 为 0ms | 必须自己控制连接生命周期；只用手动 `TcpStream` + `TlsConnector`，不复用（或明确标记复用） |
| **Happy Eyeballs v2** | 双栈连接时可能尝试 IPv6 失败后等 IPv4，增加 2-3ms | 在 DNS 阶段确定地址族，直接连解析到的地址 |
| **NAT / 防火墙** | TCP 连接可能被中间设备延迟（透明代理、NAT 端口映射） | 属于正常网络延迟，如实测量 |
| **非标准端口** | API 可能用自定义端口（非 443） | 从 URL 解析端口，TcpStream::connect 支持 |

### 3. TLS 阶段 (T2 → T3)

**如何测量**：`tokio-rustls::TlsConnector` 的 `connect` 方法在握手完成时返回。

**问题清单**：

| 问题 | 影响 | 对策 |
|------|------|------|
| **TLS 会话复用 (Session Resumption)** | 如果之前有过连接，第二次握手可省略证书交换（0-RTT 或 1-RTT） | 每次新建 `ClientConfig` 不缓存 session；或明确标注 `(resumed)` |
| **TLS 1.3 vs 1.2** | 1.3 握手少 1 RTT（1-RTT vs 2-RTT），差别显著 | 锁定 TLS 1.3（rustls 默认 1.3+1.2），如果支持就固定用 1.3 |
| **ALPN 协商** | HTTP/1.1 vs HTTP/2 的 ALPN 协商耗时 | 指定 `alpn = &[b"h2", b"http/1.1"]`，让 rustls 做正常的 APLN |
| **证书链长度** | 长证书链（intermediate CA 多）增加握手时间 | 正常测量，不干预；可在结果中标注 key 交换算法和证书链长度 |
| **OCSP Stapling** | 部分服务端会 stapling OCSP 响应，增加服务端处理时间 | 正常包含在 TLS 握手时间内 |
| **SNI** | 必须发送 SNI 扩展，否则 Cloudflare 等 CDN 会拒绝 | 用 `rustls::ClientConfig` 时会自动发送 SNI |

### 4. HTTP 首字节 (T3 → T4)

**如何测量**：TLS 连接建立后，发送 HTTP 请求，收到第一个响应字节。

由于我们要自己控制连接，不走 reqwest，这意味着需要手动发送 HTTP 请求并解析响应。

**问题清单**：

| 问题 | 影响 | 对策 |
|------|------|------|
| **HTTP 请求体大小** | 大 prompt 的 JSON 序列化和传输耗时 | 总耗时包含此部分，但对 prompt 大的情况影响显著；记录请求体大小 (bytes) |
| **HTTP/1.1 与 HTTP/2** | 帧格式不同，H2 多路复用但首字节可能受流优先级影响 | 通过 ALPN 协商决定，记录使用版本 |
| **服务端排队** | 服务端收到请求后先排队等 GPU，此时间段包含在 T3→T4 中 | 无法区分"排队"和"真实处理"，但 `服务端 TTFT = T5 - T3` 可以剥离网络纯时延 |
| **HTTP 代理/反向代理** | API 前可能有 Cloudflare/nginx 等代理 | 记录 `via` 响应头信息 |
| **响应头大小** | `Set-Cookie` 等大响应头可能延迟第一个 body chunk | T4 是响应头到达时间，不影响 TTFT |

### 5. 首 Token (T4 → T5 / T0 → T5)

**如何测量**：从 HTTP 响应体中解析 SSE，收到第一个有实际内容的 data 事件。

**问题清单**：

| 问题 | 影响 | 对策 |
|------|------|------|
| **Anthropic Ping 事件** ⚠️ | Anthropic 显式发送 `event: ping` 作为心跳，这是空事件不含 token | 必须解析 SSE event type：`ping` 事件忽略，只计 `content_block_delta` 且 `delta.type == "text_delta"` |
| **Anthropic `content_block_start`** | `content_block_start` 事件不含实际文本，`content` 为空 | 不把此事件当成首 token，其后的 `content_block_delta` 才算 |
| **OpenAI 空 choices 的 usage chunk** | `stream_options: {"include_usage": true}` 时最后会发一个 `choices: []` 的 chunk | 全程按 `choices[0].delta.content` 是否存在判断 |
| **非流式请求不需要** | `stream: false` 时整个响应体一次返回，无逐 token 数据 | 支持 `--no-stream` 模式，只测端到端总耗时 + 响应体大小 |
| **多 choice 并行返回** | `n > 1` 时每个 choice 的首 token 时间不同 | 默认 `n = 1`，或报告每个 choice 的首 token 延迟 |
| **Anthropic 多 content block** | tool_use 和 text 在同一请求中交错 | TTFT 只计第一个 text_delta |

### 6. Streaming 吞吐 (T5 → Tn)

**如何测量**：首 token 到最后一个 token 之间的间隔，配合累计字符数 / 估算 token 数。

**问题清单**：

| 问题 | 影响 | 对策 |
|------|------|------|
| **多个 token 在一个 chunk 中** ⚠️ | OpenAI 等 API 可能在一个 SSE chunk 中返回多个 token（"current weather" 一次返回），导致 chunk 到达间隔远大于 token 生成间隔 | 不能用 "每 chunk 间隔 = 单 token 延迟"。方案：① 按字符数估算 tokens，② 使用 `include_usage` 获取精确 token 数，③ 记录累计 arrived 曲线而非单点 |
| **Anthropic thinking blocks** | `thinking_delta` 在 text 之前发，这些不在最终输出中但算延迟 | 需要区分 thinking 和 text 的 token。TTFT 是第一个 text_delta。throughput 可以分开算 thinking 和 text |
| **网络抖动** | 中间网络的 TCP 拥塞控制可能导致 chunk 到达不均匀 | 记录逐 chunk 到达间隔分布（p50/p95/p99） |
| **Chunk 合并** | 操作系统 TCP 栈的 Nagle 算法/Cork 可能导致小 chunk 被合并后批量到达 | `TCP_NODELAY` 在服务端控制，客户端无法干预；如实记录 |
| **流结束判定** | 需要区分"最后内容 chunk"和"终止标记" | 对 OpenAI 是 `data: [DONE]`，对 Anthropic 是 `message_stop`，对通用 API 是 SSE 流关闭 |

### 7. Token 计数问题

| 问题 | 影响 | 对策 |
|------|------|------|
| **客户端无法知道 token 边界** | 不同的 tokenizer 对同一文本产生不同 token 数 | 方案 A（默认）：使用字符数作为粗略度量，标记为 `chars/s`。方案 B：用 `tiktoken-rs` 在客户端估算 token。方案 C（OpenAI）：`include_usage` 获取精确计数 |
| **Anthropic 不返回 usage 在流中** | Anthropic 只在 `message_delta` 中返回 cumulative usage | 记录 `message_delta` 的 `usage.output_tokens` 作为精确值 |
| **ChatGLM / 国内 API 兼容性** | 各家 tokenizer 不同，可能没有 `include_usage` | 用字符数或自动选择 tokenizer |

### 8. 连接复用与 Warmup

| 问题 | 影响 | 对策 |
|------|------|------|
| **DNS 缓存** | 后续请求 DNS ≈ 0 | 首次标记 `cold`，后续标记 `warm`；支持 `--flush-dns` |
| **TLS 会话复用** | 后续请求跳证书验证 | 所有 TCP/TLS 阶段每次重建（不缓存 session），得到的是"cold connect"真实值 |
| **HTTP Keep-Alive** | 同一连接多次请求避免 TCP/TLS 开销 | 明确每次新建连接（除非 `--keep-alive` 模拟真实场景） |
| **Warmup 请求** | 服务端冷启动（GPU 加载）影响首个 TTFT | 支持 `--warm N` 先发 N 次 warmup（不计入统计，用于预热负载均衡器/GPU） |

### 9. 各 API Provider 的 SSE 差异

| Provider | SSE 格式 | Ping/Keep-alive | Token 计数 |
|----------|----------|-----------------|------------|
| **OpenAI** | `data: {...}\n\n` 每行一个 chunk | 无显式 ping，但有 `data: [DONE]` 终止 | `include_usage` 在最终 chunk 给出 |
| **Anthropic** | `event: X\ndata: {...}\n\n` 命名事件 | **有 `event: ping` 的心跳** | `message_delta` 中有 cumulative usage |
| **Google Gemini** | `data: {...}\n\n` 类似 OpenAI | 可能有空行心跳 | 返回 `usageMetadata` |
| **Ollama** | `{"response": "..."}\n` per line | 无 | 无，需客户端估算 |
| **通用 OpenAI 兼容** | 通常与 OpenAI 一致 | 可能有差异 | 需 fallback |

---

## 架构决策

### 核心权衡：低层控制 vs 开发效率

| 方案 | 控制粒度 | 工作量 | 推荐 |
|------|---------|--------|------|
| **A: 全手工** — 自己 `TcpStream` + `rustls` + 手写 HTTP/SSE 解析 | 完全控制每阶段 | 中（~600 行核心） | ✅ 推荐 |
| **B: 混合** — `reqwest` 自定义 Connector hook | 可以拦截各阶段，但 rx_buffer 细节不够 | 中（~400 行 + 了解 Connector trait） | 备选 |
| **C: 分层** — 先单独 ping 网络（ICMP/TCP），再 reqwest 发 HTTP | 网络延迟和 HTTP 延迟不能一一对应 | 少（~300 行） | 不推荐 |

选择方案 A 的理由：
1. **精确** — 每个阶段打时间戳，没有 HTTP 客户端库的内部缓冲干扰
2. **SSE 解析** — 可以逐字节解析，区分 `ping`/`delta`/`done` 事件
3. **无依赖黑箱** — 知道你测量的每一毫秒是什么
4. **你是 sshping 作者**，这套路你最熟悉

### 依赖选择

```toml
[dependencies]
# 运行时
tokio = { version = "1", features = ["full"] }
# TLS
rustls = "0.23"
tokio-rustls = "0.26"
# DNS
hickory-resolver = "0.25"
# HTTP 请求序列化（发送 raw HTTP 用，不依赖 reqwest）
http = "1"
# JSON
serde = { version = "1", features = ["derive"] }
serde_json = "1"
# URL 解析
url = "2"
# CLI
clap = { version = "4", features = ["derive"] }
# 表格输出
tabled = "0.18"
# 统计
stats = "0.2"  # 或直接手算 min/max/avg/p50/p95/p99
# 可选：tiktoken-rs 用于精确 token 估算
# tiktoken-rs = "0.6"
```

### 模块划分

```
src/
├── main.rs       — CLI 入口 + 主循环
├── probe/        — 逐层探测
│   ├── mod.rs
│   ├── dns.rs    — DNS 解析 + 计时
│   ├── tcp.rs    — TCP 连接 + 计时
│   ├── tls.rs    — TLS 握手 + 计时
│   └── http.rs   — HTTP 请求 + SSE 流解析 + 逐 token 计时
├── sse.rs        — SSE 事件解析器（事件驱动，通用）
├── stats.rs      — 统计量计算（min/max/avg/p50/p95/p99）
└── display.rs    — CLI 输出格式
```

### CLI 接口草案

```
USAGE:
    llm-ping [OPTIONS] --url <URL> --model <MODEL>

OPTIONS:
    -u, --url <URL>              API 端点 (e.g. https://api.openai.com/v1/chat/completions)
    -m, --model <MODEL>          模型名 (e.g. gpt-4o, claude-3-opus)
    -p, --prompt <PROMPT>        提示词文本（默认: "Say hello in 20 words."）
    -k, --api-key <KEY>          API key（默认: $OPENAI_API_KEY / $ANTHROPIC_API_KEY）
    -c, --count <N>              请求次数 [default: 1]
    --stream / --no-stream       是否流式 [default: stream]
    --warm <N>                   Warmup 请求次数 [default: 0]
    --timeout <SECS>             超时 [default: 60]
    --flush-dns                  每次请求重新解析 DNS
    --json                       JSON 输出（供脚本消费）
    -v                           详细日志
```

---

## 里程碑

1. **M1: 单次 cold connect 测量**
   - 实现 DNS → TCP → TLS → HTTP raw request → 非流式单次响应
   - 输出各阶段耗时表格
   - ~400 行核心

2. **M2: Streaming + SSE 解析**
   - 解析 SSE 事件流
   - 过滤 ping/keep-alive 事件
   - 测量 TTFT + 逐 token 到达间隔
   - ~200 行新增

3. **M3: 多次请求 + 统计**
   - `--count N` 循环
   - warmup 机制
   - p50/p95/p99 分布统计
   - ~150 行新增

4. **M4: 多 Provider 适配**
   - Anthropic 格式（命名事件）
   - Gemini / Ollama / generic OpenAI-compatible
   - ~150 行新增

5. **M5: JSON 输出 + 可脚本化**
   - `--json` 输出
   - 非交互式模式
   - ~100 行新增
