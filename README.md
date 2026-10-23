# MCP Commander

Current build: **2026.54** (`2026.54.0` in Cargo and macOS version metadata).

Russian build and command guide: [BUILD_AND_COMMANDS.md](BUILD_AND_COMMANDS.md).

A local macOS app written in Rust. It can chat with Ollama, OpenAI-compatible APIs, Anthropic, Gemini, or DeepSeek and connect to local or HTTP MCP servers. The model can request MCP tool calls; the app asks for approval before each one by default. Commander mode can assign tasks to multiple implementers.

## Run

Build the app with `./build-app.sh`, then open `dist/MCP Commander.app`. The default Ollama endpoint is `http://127.0.0.1:11434`; change it in **Connections** if Ollama runs on another computer. Click **Refresh Ollama model list** to choose an installed model for a role.

Click **Check for updates** in the sidebar to compare the installed version with the latest published GitHub Release. The result links to the release page when a newer version is available. Checking does not download or install software. Until the first release is published, the app reports that no release is available.

Turn on **Commander mode** in **Agent roles**. Choose a provider and model for the commander, then choose a provider, model, and quantity (1–4) for each of two implementer groups. The main area splits into commander chat on the left and an implementer dashboard on the right. The dashboard shows each agent's command, status, and result, plus available counts. The commander receives `delegate_to_implementer`, can assign independent tasks in parallel, and waits for their results before responding. Implementers use their own configured model and the enabled MCP servers; they cannot delegate further. **Stop** cancels the current request and its implementers. Turn off Commander mode for a single-agent chat.

For a cloud provider used by any role, select it under **Connections**, configure its endpoint and API key, then click **Save key**. The app stores a separate key for each cloud provider in the local macOS Keychain. Click **Check key** to verify the key using the provider's model-list endpoint without generating a response. Click **Delete saved key** to remove it. DeepSeek defaults to `https://api.deepseek.com`; choose `deepseek-flash` or `deepseek-v4-pro` for its roles. API keys are never written to the settings file.

Press **Enter** in the message field to send, or **Shift+Enter** for a new line. **Stop** cancels the current response and prevents further agent steps. A tool action already sent to an MCP server may still finish on that server.

**Attach files** supports UTF-8 text files and PNG/JPEG/WebP images. **Paste clipboard** appends clipboard text to the message or attaches a clipboard image as PNG. Command+V pastes text at the cursor or attaches an image when the clipboard contains no text. Images are sent using each assigned role's provider format. Use **Remove** to drop a pending attachment before sending. PDF and other binary documents are not supported.

To connect a program, click **+** under MCP servers. Choose **Local command** for a program installed on this Mac; enter the executable and put each command-line argument on its own line. Choose **HTTP URL** for a Streamable HTTP MCP server. Use **Delete** beside a server to remove it. Click **Check MCP connections** to list the available tools. The app asks you to allow or deny every tool call by default. Turn on **Run MCP tools without asking** to let the model call tools on enabled servers without individual prompts. This setting is saved and applies to future requests.

The non-secret configuration is stored at `~/Library/Application Support/MCP Commander/config.json`. Existing settings are migrated to the two-group layout and Keychain entries are reused. Conversations are only kept in memory and are cleared when the app exits or you start a new chat. macOS may ask you to allow Keychain access after an app update.

## Build

Requires Rust and the macOS command-line tools. From this directory, run `./build-app.sh`. The resulting `.app` is in `dist/`.

## Limits

- MCP HTTP supports Streamable HTTP URLs without custom authentication headers. Local `stdio` servers run as child processes on this Mac.
- Model replies are shown after each request completes; token-by-token streaming is not implemented.
- Up to eight consecutive tool rounds are allowed for each message.
