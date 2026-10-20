# Architecture and reading guide

## From the original router

The original program used typed roles, a `VecDeque<Message>` router, a `handle` function, and CEO report slots. Its CEO state was constructed inside the queue loop and lost previous reports. This version keeps those ideas but gives each run one engine and one `CeoState`, with a set of required specialists and a map of their outcomes. `Engine::execute` handles the async routing cases that previously lived in `handle`.

Read the modules in this order:

| Module | Responsibility |
| --- | --- |
| `message.rs` | Role/kind/status enums, UUID records, timestamps, valid route rules |
| `state.rs` | Immutable execution events, agent states, required reports, exactly-once finalization guard |
| `config.rs` | Snapshot of owner context, edited role prompts, role models, concurrency and peer settings |
| `evaluation.rs` | Synthetic role probes, transparent deterministic scoring, cancellable evaluation worker |
| `app/settings.rs` | Inline context/prompt/model editors and Model Lab |
| `tools.rs` | Role allowlists, Ollama JSON schemas, strict `{}` argument validation, fictional fixtures or owner-provided section dispatch |
| `agent.rs` | Per-role system prompts and independently owned chat histories |
| `ollama.rs` | Native Ollama wire types, bounded HTTP requests, discovery, capabilities, response validation |
| `orchestrator.rs` | Run lifecycle, FIFO delivery, model/tool loops, cancellation, demo scenario |
| `app.rs` | Event reducer, UI controls, Painter graph/animations, timeline and inspectors |
| `main.rs` | Logging, Tokio runtime, native window creation |

## Execution flow

1. Start clones the editable RunOptions and creates a run UUID, fresh agent contexts, and a watch channel for cancellation. The GUI spawns one async task on its Tokio runtime.
2. In live mode, `/api/show` checks the selected model's tool capability. The owner task is emitted as **Sent**, queued, and then emitted as **Delivered** when the engine pops it. Delivery means the recipient accepted the message; it does not mean inference has completed.
3. CEO becomes Working. Its history receives the task and planning instructions. Tool requests are validated and executed locally; each call and result is a separate event. The assistant reply is preserved exactly in history.
4. The CEO's structured assessment/delegations are validated. Only unique specialist roles with nonempty tasks are accepted. The complete original JSON is sent to Owner as an Assessment; delegated tasks enter the queue. The CEO becomes Waiting.
5. Specialist tasks are delivered FIFO. Each specialist gets its own prompt/history and tool schemas. A semaphore limits worker concurrency to 1–4. The coordinator moves each agent into one JoinSet worker and takes it back with its result; no two requests share a mutable history. When peer consultation is enabled, drafts are held until the assessment/question/answer/revision phases finish. Each successful final response becomes a Report; each error becomes a Failure report. Reports are queued in role order after all selected workers finish.
6. CEO accepts only required, first-time reports for this run. A failure counts as an outcome, so a failed specialist cannot deadlock the run. Once all outcomes arrive, `begin_finalization` locks the finalization guard before the synthesis request begins.
7. CEO receives all required outcomes in its own history and produces the final synthesis with further tool schemas disabled. Tool-shaped JSON returned as report text triggers a corrective prompt within the same six-response budget; the original response is retained in history. Only the delivered Final message populates the decision desk. A run has one final report if successful; cancellation or CEO failure produces zero, never fabricated fallback output.
8. A terminal Finished event marks the run complete, failed, or cancelled. The UI resolves remaining active states and ignores events after termination or from another run.

## Ownership and channels

`Engine` owns `BTreeMap<Role, Agent>` and the queue. Each agent owns its `Vec<ChatMessage>`; histories are never shared across roles or runs. Awaiting HTTP borrows the selected agent's history immutably while the response is pending. After the response arrives, the engine records it with a mutable borrow. No mutex around chat history is needed: the coordinator transfers ownership into a worker, then receives the agent back after its turn.

A standard library MPSC channel transfers **owned immutable event snapshots** to the UI. The worker calls egui's thread-safe `request_repaint` after sending an event. `App::update` only drains ready events, never waits for network I/O. It applies up to 128 events per frame and repaints while running. This keeps slow inference off the GUI thread.

A Tokio watch channel carries the stop signal. `tokio::select!` races cancellation against the entire run future wrapped in a 15-minute timeout. Dropping the run future drops its JoinSet, which aborts owned workers and their in-flight requests or demo delays. Request-level timeouts bound individual operations. Closing the app signals cancellation and shuts down the runtime in the background.

The channel preserves per-worker causal order; events from concurrent roles may interleave. The coordinator remains the only router. A message's Sent and Delivered events share its UUID; the events themselves have separate UUIDs/timestamps. Tool call/result events share a call UUID. Graph pulses are a visualization of already-received events, not a transport simulation; animation duration never invents delivery. Multiple quick events may animate together.

The engine uses a barrier between draft, peer-assessment, peer-answer, revision, and CEO synthesis phases. Each specialist may request zero to two distinct selected peers. Queries are grouped by recipient so even mutual consultations cannot deadlock or mutate the same history concurrently. Reply UUIDs must exactly match the requested IDs; missing, duplicate, unexpected, or empty answers become visible consultation failures. A reply message carries `reply_to`, linking to the question message. There is one round and no recursive peer invocation.

## Tool protocol and trust boundary

The tool catalogue is deliberately small. Every tool is a pure function returning `serde_json::Value`. Fixture mode returns `fictional: true`; owner mode returns `fictional: false`, a source label, the unverified original section text, and an availability flag. Missing owner data never falls back to fixtures. All schemas require an object with no properties; dispatch independently enforces role authorization and an exactly empty object. Unknown/unauthorized tools and malformed arguments yield error results, which the model can see and correct within its bounded budget.

With native tool support, assistant `tool_calls` remain in history and each dispatch result is appended as a `tool` message with `tool_name`. Multiple calls are executed in their returned order. Without native tool support, the harness invokes permitted tools for the selected data source and records their results as explicitly identified user-context evidence. Both paths emit tool call/result events. Demo uses the same tools and event plumbing but labels every scripted agent reply.

The HTTP client only accepts loopback hostnames/IPs over HTTP, bypasses proxies, and refuses redirects. Known cloud model tags and remote-backed model metadata are rejected. No tool performs I/O. Role restrictions are checked in Rust, not entrusted to prompts. System prompts instruct agents to treat reports and data as evidence and to make any mutation a proposal for human approval. There is intentionally no executable mutation tool.

## UI and state

`ViewState::apply` is the reducer. It holds the UI's independent mirror of agent histories and statuses; it cannot drive inference. Message bodies remain complete and unmodified. Timeline previews are short, while the inspector shows the original body or full event JSON. Agent histories retain original assistant content, tool calls, tool results, and optional thinking returned by Ollama.

`Painter` uses a stable six-node layout. Paired offset arrows distinguish directions. Gold pulses indicate Sent; mint pulses indicate Delivered. Agent boxes show Idle, Working, Waiting, Completed, or Failed. Cancelled active agents return to Idle; the run itself remains labelled Cancelled. Tool events are purple, with failures red. Every terminal result stays inspectable until Clear, Reset, or Start.

## Boundaries and tests

Tests exercise routing rules, run isolation, state transitions, duplicate/late reports, finalization, authorization, malformed tool arguments, invalid plans, demo execution order, and cancellation. Local mock HTTP tests exercise Ollama wire behavior and engine failure handling without requiring a GPU or model. The native window still needs a real graphical environment to assess rendering; model reasoning needs a real installed model.

SQLite persistence is optional scope and is not included in this MVP. No hidden file writes, telemetry, external service calls, or real-world actions occur. All run state, settings drafts, and evaluation results are in memory and structured logs contain event/run IDs rather than full task content.

## Evaluation design

The Model Lab sends five independent synthetic cases per selected model, using default role prompts and no owner context. It requests structured JSON and scores five explicit checks per response. An exact decision vocabulary is enforced by the response schema; CFO's arithmetic check compares the reported runway with 205000 / 28000 within 0.02 months. Full responses remain available so users can judge nuance that deterministic checks miss. The evaluator uses its own watch cancellation channel and result channel, sequential requests, a 30-minute deadline, and the same bounded Ollama client. It does not download models, benchmark tools, or claim a production-quality ranking.
