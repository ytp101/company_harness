# Company Harness

A native Rust dashboard for studying a small AI company: Owner → CEO → specialists → CEO → Owner. The graph, timeline, tool inspector, and final report reflect actual ordered engine events. Choose **fictional fixtures** or **owner-provided company context**. Tools are read-only pure functions; agents cannot execute shell commands, access files, or take real-world actions.

## Run

Use Rust **1.88 or newer** (tested with 1.91). On macOS, install the Xcode command-line tools if needed (`xcode-select --install`).

```sh
cargo run
# Explore the offline, explicitly scripted launch-review scenario:
cargo run -- --demo
```

`--demo` selects Demo Mode; click **Start** to run. Add `--autorun` to start the demo immediately. Demo output is scripted regardless of the task you enter. It exercises the same router, event channel, state tracking, and real fixture tools as the live mode, without calling an LLM.

## Local Ollama

1. Install [Ollama](https://ollama.com/download) and start its macOS application, or run `ollama serve`. Do not start a second server if the app already serves port 11434.
2. Pull a chat model suitable for your machine, for example `ollama pull qwen3:8b`. Model memory needs vary; a smaller model may be more practical on a laptop. See [Ollama's tool-calling documentation](https://docs.ollama.com/capabilities/tool-calling).
3. Run `cargo run`, choose **Ollama Mode**, and click **Refresh models**. Select an installed model or edit the model name directly.
4. Enter `Evaluate whether our fictional SaaS should launch next month` and click **Start**.
5. Watch messages move through the graph. Click a timeline row for the original full message or tool arguments/result. Click an agent for its system prompt, tools, and full independent history. Open **CEO final report** for the synthesis.

The default endpoint is `http://localhost:11434`; connection settings allow local HTTP endpoints only. Discovery uses `/api/tags`, capabilities use `/api/show`, and inference uses `/api/chat` with `stream: false`. Native tool-capable models receive tool schemas and return actual `tool_calls`; results are appended as `role: tool` messages. If `/api/show` does not advertise tools, the harness explicitly logs and supplies permitted selected company data as context instead. No scripted answers are used in Ollama Mode.

The CEO uses a structured JSON response to select 1–4 specialists. A model that cannot follow the schema produces a visible failure rather than fabricated delegation. A missing model, unsupported API, empty response, malformed response, or request failure is visible in the timeline. Specialists can fail independently; the CEO is still asked to synthesize the available evidence and disclose failures. If the CEO itself fails, no fake final report is produced.

## Your own context and prompts

Open **Company context & agents**, enable **Use my own company context**, and enter your shared company profile plus roadmap, finances, marketing, and policy sections. Choose **Ollama Mode** to analyze this context. Empty sections produce an explicit unavailable/unknown result; they never inherit Northstar sample numbers. Company profile is shared with all roles; specialized sections remain behind the existing tool allowlists. Peer replies can intentionally share findings across roles.

Expand a role to edit its system prompt inline and choose its model. A blank role model inherits the main selector. You can also edit a role's prompt in its graph inspector. **Start snapshots all settings**; active histories are never rewritten by later edits. Prompts cannot grant additional tool permissions. All data and edits remain in memory, and no documents are read automatically. Demo Mode cannot run with owner context because its answers are scripted.

## Async execution and peer consultation

In **Company context & agents**, choose concurrency from 1–4 and enable peer consultation. Specialists first draft their analyses. Each successful specialist then uses its own model to decide whether to ask up to two other selected specialists a question, or proceed without consultation. Questions and correlated replies appear as separate messages in the timeline and animate along peer edges. Each recipient answers its batch of questions using its own history. Requesters revise their reports before the CEO synthesizes them.

One peer round prevents recursive loops and deadlocks. Failed assessments/answers are disclosed; no fake peer answer is inserted. Revision failure becomes a specialist failure. The CEO still receives exactly one terminal report per selected specialist. Cancel aborts all pending workers, including peer requests.

This is concurrent async scheduling in Rust, not a promise of parallel GPU execution. Ollama may queue requests depending on server configuration and memory. With several different role models, weight swapping can outweigh concurrency benefits; begin with one shared model. See [Ollama's concurrency FAQ](https://docs.ollama.com/faq).

## Model lab

Open **Model lab & recommendations** for candidate models, source links, and copyable `ollama pull` commands. Pull a model yourself, refresh the installed-model list, select candidates, and run the role evaluation. You can assign a tested model to a role directly from its result card.

Each model gets one synthetic probe for CEO, CTO, CFO, CMO, and Legal, using default prompts and public synthetic evidence, never your company context. Each result shows five explicit checks, latency, errors, and the full answer. These checks assess schema compliance, a prescribed decision vocabulary, evidence IDs, uncertainty, and CFO arithmetic. They **do not establish a universal best model**, measure tool-calling reliability, or replace human review of reasoning. Results are deliberately labelled synthetic and not converted into a misleading overall ranking. Evaluations run sequentially and can be cancelled independently; company runs and evaluation cannot run together in the UI.

For the current 16 GB Mac, start by comparing `qwen3.5:4b` with `qwen3.5:9b` at concurrency 1. These are candidates, not locally measured winners. See [MODEL_GUIDE.md](MODEL_GUIDE.md) for the role-by-role rationale and source links. No model is downloaded automatically.

## Controls and limits

- **Start** starts a fresh run with fresh agent contexts; the previous run is replaced.
- **Cancel** drops the current async HTTP future and stops further routing. Ollama may still complete computation server-side; cancellation does not unload the model.
- **Clear** removes the inactive run, events, and histories, retaining configuration and task text.
- **Reset** also restores the default task, Ollama mode, endpoint, context, prompts, and per-role model overrides, then refreshes models. Clear/Reset/configuration changes are disabled while running.
- Each request has a 120-second timeout (3-second connection timeout); discovery has an 8-second limit and runs have a 15-minute total deadline.
- Each agent turn allows 6 model responses, at most 5 tool rounds and 8 tool calls per round. Responses are capped at 2 MB, generated output at 2,048 tokens, context at 16,384 tokens, and owner input at 16,000 bytes.
- The GUI defaults to two concurrent specialist workers; set 1–4 in Company context & agents. CEO planning and synthesis remain ordered. Slow local models leave the GUI responsive. Long thinking responses can hit the output limit; try another model or a smaller task.
- Timeline clock labels are explicitly UTC; event details retain exact Unix milliseconds and UUIDs. Sent/delivered rows share a message ID but have distinct event IDs and execution timestamps.

## Headless verification

The same engine can run without a window, using the acceptance-test launch goal:

```sh
cargo run -- --headless --demo
cargo run -- --headless --model llama3.2:latest --parallel 2 --peers
cargo run -- --evaluate --model llama3.2:latest
```

These commands print complete delivered messages and tool outcomes after the run and exit nonzero if the run fails. The GUI Cancel control is available only in windowed mode.

## Develop and study

```sh
cargo fmt --check
cargo check --locked
cargo test --locked
cargo clippy --all-targets --locked -- -D warnings
RUST_LOG=company_harness=info cargo run -- --demo
```

Start with [ARCHITECTURE.md](ARCHITECTURE.md), then `message.rs`, `state.rs`, `orchestrator.rs`, `tools.rs`, `ollama.rs`, and `app.rs`. The code evolves the original `Role`, `MessageKind`, `VecDeque<Message>`, and CEO report state into separate modules. The original bug of resetting CEO state inside the queue loop is removed.

The dependency lockfile is included for reproducibility. eframe/egui 0.33.3 is used because it supports this Rust toolchain; the newer 0.37 release requires Rust 1.95. The app uses the documented 0.33 `App::update` API and Glow renderer.

## Scope

This MVP keeps one run in memory. Optional SQLite persistence, run replay, token streaming, and real-world action execution are not implemented. Closing the app discards conversations, context, edited prompts, model assignments, and evaluation results. Recommendations are proposals for human review within the simulation. LLM judgments and tool-use quality depend on the selected model; automated mock-server tests verify the harness protocol, not model reasoning quality.

## Verification performed

- `cargo fmt --check`, `cargo check --locked`, `cargo test --locked` (23 tests), and strict `cargo clippy --all-targets --locked -- -D warnings`.
- Native macOS Demo Mode launched and completed; dashboard, configuration editor, and Model Lab rendering inspected from captured application frames.
- A real local `llama3.2:latest` run completed the launch-evaluation flow using Ollama and produced a plain-language final CEO synthesis. This is a smoke test, not a guarantee of model reasoning quality.
- Mock HTTP tests verify native tool round trips, failed-specialist handling, separate contexts, plain-text tool-call correction, non-tool-model evidence injection, response validation, and cancellation during a pending request.

Additional tests cover settings snapshots, owner-data separation, per-role model routing, bounded concurrent workers, parallel cancellation, peer route validation, UUID-correlated real HTTP peer answers, and the evaluation rubric.
