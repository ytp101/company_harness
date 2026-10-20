# Choosing a model per role

Recommendations checked against official Ollama model pages on 2026-10-19. These are **candidates to compare**, not measured winners. The machine used for development has 16 GB unified memory. Model weights, context/KV cache, concurrent requests, and macOS all share that memory; a download's size is not its total runtime footprint.

## Start with one shared model

Separate agent histories provide role specialization even when every role uses the same weights. Begin with:

```sh
ollama pull qwen3.5:4b
```

Compare against a larger candidate at concurrency 1:

```sh
ollama pull qwen3.5:9b
```

An alternative baseline:

```sh
ollama pull qwen3:8b
```

The official [Qwen3.5 4B](https://ollama.com/library/qwen3.5:4b), [Qwen3.5 9B](https://ollama.com/library/qwen3.5:9b), and [Qwen3 8B](https://ollama.com/library/qwen3:8b) pages list tool support. Keep Ollama current enough to support the chosen family. This project does not download any model automatically.

Avoid loading several large models at once on this machine. A role override is useful after you observe a concrete improvement that justifies extra latency/memory. Ollama may queue concurrent requests or swap models; see its [server concurrency and memory FAQ](https://docs.ollama.com/faq).

## What to compare by role

The proposed mappings below are engineering judgments about what to test, not benchmark findings proving one family best.

| Role | Initial candidate | Comparison candidate | Evidence that should decide |
| --- | --- | --- | --- |
| CEO | qwen3.5:4b | qwen3.5:9b | Valid delegation, sensible specialist selection, honest synthesis of failures, actionable decision |
| CTO | qwen3.5:4b | qwen3.5:9b | Technical reasoning, real blockers vs assumptions, useful requests to Legal/CFO |
| CFO | qwen3.5:4b | qwen3.5:9b | Correct arithmetic, units, periods, runway, uncertainty; independently verify numerical claims |
| CMO | qwen3.5:4b | qwen3:8b or qwen3.5:9b | Clear proposals, recognition of weak samples, no invented market evidence |
| Legal | qwen3.5:4b | qwen3.5:9b | Identifies missing jurisdiction and approvals, avoids unsupported legal certainty, asks relevant peers |

A larger model is not guaranteed to outperform a smaller one on your prompts. There is no universal “best finance/legal model” established by this project. The role probes are intentionally small and should be supplemented with representative tasks and human review.

## Reproducible comparison

1. Pull a candidate, then use **Refresh models**.
2. Open **Model lab & recommendations**, select installed candidates, and choose **Run role evaluation**.
3. Inspect each role's five checks, response time, raw answer, and errors. Scores test explicit schema/vocabulary, evidence IDs, uncertainty, and one financial calculation. They do not assess all reasoning quality, actual tool behavior, or your custom prompts.
4. Use **Use this model for this role** only after reviewing the result. A blank role-model field means use the shared main model.
5. Test your actual company goal with the desired edited prompts and context. Compare the complete reports and peer questions, not just the synthetic score. Repeat trials before choosing a model; this harness's evaluation is one sample per role.

CLI equivalent (sequential, synthetic evidence only):

```sh
cargo run -- --evaluate --model llama3.2:latest --model qwen3.5:4b
```

Only run that command after both models are installed. Evaluation never pulls missing models and never uses your company context. UI results are kept in memory; capture CLI stdout yourself if you want a comparison record.

## What was actually exercised here

Only `llama3.2:latest` was installed. A live company run with concurrency 2 completed: all four specialists evaluated whether they needed a peer; Legal asked CTO a question, CTO answered, and Legal revised before the CEO finalized. This confirms the execution path works with that local model, not that its conclusions are correct. A separate synthetic probe exposed an omitted CFO calculation, demonstrating why raw-answer inspection matters. Qwen candidates above have not been downloaded or benchmarked on this machine.

One recorded local probe run using the current schema (2026-10-19):

| llama3.2:latest role | Checks passed | Request time | Notable failure |
| --- | --- | --- | --- |
| CEO | 5/5 | 3.4 s | No check failed; broader synthesis quality is not measured |
| CTO | 3/5 | 2.5 s | Chose launch despite blockers; added a non-existent evidence ID |
| CFO | 4/5 | 2.0 s | Returned null instead of the requested 7.32-month runway |
| CMO | 4/5 | 2.0 s | Chose scale instead of the expected limited pilot |
| Legal | 4/5 | 1.8 s | Chose launch despite incomplete approvals |

A 4/5 here can still contain a major decision error. It means four mechanical checks passed, not “80% reliable.” Latencies are single warm/local measurements and not hardware-independent. The CEO's 5/5 is not evidence that this model is the best CEO model. Rerun on your chosen candidates and inspect every response.
