# Format code
format:
    cargo +nightly fmt --all

# Clippy
check: format
    cargo clippy --fix --allow-staged --all-targets
    @just format

# Unit tests
test: check
    cargo test

# Quick local test with Ollama
run-local:
    cargo run -- -t ollama -m gemma4:12b -p "Hi" -c 1
