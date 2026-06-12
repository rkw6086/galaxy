use local_inference::{
    Device, InferenceEngine, InferenceTask, InputClassificationInput, InputClassificationTask,
    TabNamingInput, TabNamingTask,
};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    env_logger::init();

    println!("Initializing inference engine (this may download the model on first run)...");
    let engine = InferenceEngine::new(Device::best_available()).await?;
    println!("Engine ready!\n");

    // --- Input Classification ---
    let test_inputs = vec![
        "ls -la",
        "git status",
        "what files are in this directory?",
        "explain this error to me",
        "docker compose up -d",
        "how do I fix this segfault?",
        "cd /tmp && rm -rf build/",
        "refactor the auth module to use JWT",
    ];

    println!("=== Input Classification ===\n");
    for input in &test_inputs {
        let result = InputClassificationTask
            .run(
                &engine,
                InputClassificationInput {
                    user_input: input.to_string(),
                    recent_commands: vec!["git log".into(), "npm test".into()],
                    is_follow_up: false,
                },
            )
            .await?;

        println!(
            "  {:50} -> {:?} (confidence: {:.2})",
            format!("\"{}\"", input),
            result.category,
            result.confidence
        );
    }

    // --- Tab Naming ---
    println!("\n=== Tab Naming ===\n");
    let tab_name = TabNamingTask
        .run(
            &engine,
            TabNamingInput {
                recent_commands: vec![
                    "git checkout feature/auth".into(),
                    "cargo test".into(),
                    "vim src/auth/mod.rs".into(),
                ],
                working_directory: "/home/user/projects/myapp".into(),
            },
        )
        .await?;

    println!("  Suggested tab name: \"{tab_name}\"");

    Ok(())
}
