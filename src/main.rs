mod agent;
mod app;
mod config;
mod evaluation;
mod message;
mod ollama;
mod orchestrator;
mod state;
mod tools;

fn main() -> eframe::Result {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "company_harness=info".into()),
        )
        .init();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("create Tokio runtime");
    if std::env::args().any(|arg| arg == "--evaluate") {
        let args: Vec<_> = std::env::args().collect();
        let models: Vec<_> = args
            .windows(2)
            .filter(|a| a[0] == "--model")
            .map(|a| a[1].clone())
            .collect();
        if models.is_empty() {
            eprintln!("Use --evaluate --model <installed-model> [--model <another-model>]");
            std::process::exit(2);
        }
        let (tx, rx) = std::sync::mpsc::channel();
        let (_cancel, cancel_rx) = tokio::sync::watch::channel(false);
        runtime.block_on(evaluation::run(
            "http://localhost:11434".into(),
            models,
            tx,
            cancel_rx,
            None,
        ));
        for update in rx.try_iter() {
            match update {
                evaluation::Update::Result(r) => println!(
                    "{} / {}: {}/5, {:.1}s\n{}\n{:?}\n",
                    r.role,
                    r.model,
                    r.score(),
                    r.elapsed.as_secs_f32(),
                    r.response,
                    r.error
                ),
                evaluation::Update::Finished(s) => println!("{s}"),
            }
        }
        return Ok(());
    }
    if std::env::args().any(|arg| arg == "--headless") {
        if !headless(&runtime) {
            std::process::exit(1);
        }
        return Ok(());
    }
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_inner_size([1380.0, 900.0])
            .with_min_inner_size([1050.0, 760.0]),
        ..Default::default()
    };
    eframe::run_native(
        "Company Harness — Local Agent Lab",
        options,
        Box::new(move |cc| Ok(Box::new(app::CompanyApp::new(cc, runtime)))),
    )
}

/// Runs the same engine without a window for reproducible protocol smoke checks.
fn headless(runtime: &tokio::runtime::Runtime) -> bool {
    let args: Vec<String> = std::env::args().collect();
    let model = args
        .windows(2)
        .find(|a| a[0] == "--model")
        .map(|a| a[1].clone())
        .unwrap_or_else(|| "llama3.2:latest".into());
    let mode = if args.iter().any(|a| a == "--demo") {
        orchestrator::Mode::Demo
    } else {
        orchestrator::Mode::Ollama
    };
    let id = uuid::Uuid::new_v4();
    let (tx, rx) = std::sync::mpsc::channel();
    let (_cancel, cancel_rx) = tokio::sync::watch::channel(false);
    runtime.block_on(orchestrator::run(
        orchestrator::RunConfig {
            id,
            mode,
            endpoint: "http://localhost:11434".into(),
            model,
            goal: "Evaluate whether our fictional SaaS should launch next month".into(),
            options: crate::config::RunOptions {
                concurrency: args
                    .windows(2)
                    .find(|a| a[0] == "--parallel")
                    .and_then(|a| a[1].parse().ok())
                    .unwrap_or(1),
                peer_consultation: args.iter().any(|a| a == "--peers"),
                ..Default::default()
            },
        },
        orchestrator::EventSink {
            run_id: id,
            tx,
            repaint: None,
        },
        cancel_rx,
    ));
    let mut success = false;
    for event in rx.try_iter() {
        match event.kind {
            state::EventKind::Message(m) if m.status == message::MessageStatus::Delivered => {
                println!(
                    "{} → {} [{:?}]\n{}\n",
                    m.sender, m.recipient, m.kind, m.body
                )
            }
            state::EventKind::Notice(text) => println!("{text}"),
            state::EventKind::Tool {
                role,
                name,
                outcome: Some(result),
                ..
            } => println!("Tool {role}/{name}: {result:?}"),
            state::EventKind::Finished { outcome } => {
                println!("Outcome: {outcome:?}");
                success = outcome == state::RunOutcome::Completed;
            }
            _ => {}
        }
    }
    success
}

#[cfg(test)]
mod test_support;
