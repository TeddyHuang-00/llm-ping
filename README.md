# llm-ping

Detailed latency diagnostic for LLM API endpoints: measures DNS, TCP, TLS, HTTP first byte, time-to-first-token (TTFT), token generation throughput, and end-to-end total, decomposed per request.

![demo](image/demo.gif)

```
Usage: llm-ping [OPTIONS]

Options:
      --provider <PROVIDER>  Provider type [default: ollama] [possible values: ollama, open-ai, anthropic, deep-seek, open-router, ...]
  -u, --url <URL>            API endpoint URL (default from --provider)
  -m, --model <MODEL>        Model name (default from --provider)
  -p, --prompt <PROMPT>      Prompt text [default: "Introduce yourself in one short sentence."]
  -c, --count <COUNT>        Number of requests [default: 1]
      --warm <WARM>          Warmup requests (not counted in stats) [default: 0]
  -k, --api-key <API_KEY>    API key (default: provider-specific env var)
      --no-stream            Non-streaming mode
      --flush-dns            Flush DNS cache between requests
      --json                 JSON output
      --dry-run              Dry run: print request details without sending
  -v, --verbose...           Verbose output (-v: info, -vv: debug, -vvv: trace)
      --quiet                Suppress progress dots
      --timeout <TIMEOUT>    Request timeout in seconds [default: 60]
  -h, --help                 Print help (see more with '--help')
  -V, --version              Print version
```
