# llm-ping

Per-phase latency diagnostic for LLM API endpoints — measures DNS, TCP, TLS,
HTTP first byte, time-to-first-token (TTFT), token generation throughput, and
end-to-end total, decomposed per request.

![demo](image/demo.gif)

```
Usage: llm-ping [OPTIONS]

Options:
      --provider <PROVIDER>  Provider type [default: ollama] [possible values:
                             ollama, open-ai, anthropic, deep-seek, ...]
  -u, --url <URL>            API endpoint URL (default from --provider)
  -m, --model <MODEL>        Model name (default from --provider)
  -p, --prompt <PROMPT>      Prompt text [default: Introduce yourself...]
  -c, --count <COUNT>        Number of requests [default: 1]
      --warm <WARM>          Warmup requests (not counted in stats) [default: 0]
  -k, --api-key <API_KEY>    API key (default: provider env var, e.g. DEEPSEEK_API_KEY)
      --no-stream            Non-streaming mode
      --dry-run              Print request details without sending
      --json                 JSON output
  -v, --verbose              Verbose output (-v: debug, -vv: trace)
  -h, --help                 Print help
  -V, --version              Print version
```
