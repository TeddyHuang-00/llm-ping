Output image/demo.gif
Set TypingSpeed 30ms
Set FontFamily "JetBrainsMono Nerd Font Mono"
Set FontSize 16
Set Width 1200
Set Height 800
Set Theme "Catppuccin Mocha"

# Setup
Hide
Type "cargo build --release && cp target/release/llm-ping ."
Enter
Wait
Ctrl+L
Show

# Show help
Type "./llm-ping -h"
Enter
Wait
Sleep 2s

# Clear
Ctrl+L
Sleep 500ms

# Show actual output
Type "./llm-ping --provider ollama --model gemma4:12b"
Enter
Wait
Sleep 2s

# Clean up
Hide
Type "rm ./llm-ping . && clear"
Enter
Show
