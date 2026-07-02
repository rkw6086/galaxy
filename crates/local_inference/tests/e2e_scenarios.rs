//! End-to-end scenario tests that validate realistic usage patterns for
//! the local inference engine across all task types (classification,
//! prompt suggestion, tab naming).

use instant::Instant;
use local_inference::{
    Device, InferenceEngine, InferenceTask, InputCategory, InputClassificationInput,
    InputClassificationTask, PromptSuggestionInput, PromptSuggestionTask, TabNamingInput,
    TabNamingTask,
};

async fn get_engine() -> InferenceEngine {
    InferenceEngine::new(Device::Cpu)
        .await
        .expect("Failed to initialize engine — is the model downloaded?")
}

// --- Full workflow scenarios ---

/// Simulates a user session where they type various inputs and the classifier
/// routes them correctly between shell and agent modes.
#[tokio::test]
async fn scenario_user_session_mode_switching() {
    let engine = get_engine().await;

    struct TestCase {
        input: &'static str,
        recent: Vec<&'static str>,
        follow_up: bool,
        expected: InputCategory,
    }

    let cases = vec![
        TestCase {
            input: "cd ~/projects/myapp",
            recent: vec!["ls", "pwd"],
            follow_up: false,
            expected: InputCategory::Shell,
        },
        TestCase {
            input: "npm start",
            recent: vec!["cd ~/projects/myapp", "ls"],
            follow_up: false,
            expected: InputCategory::Shell,
        },
        TestCase {
            input: "why is my server crashing on startup?",
            recent: vec!["npm start", "cd ~/projects/myapp"],
            follow_up: false,
            expected: InputCategory::AgentPrompt,
        },
        TestCase {
            input: "can you also check the environment variables?",
            recent: vec!["npm start"],
            follow_up: true,
            expected: InputCategory::AgentPrompt,
        },
        TestCase {
            input: "export NODE_ENV=production",
            recent: vec!["npm start"],
            follow_up: false,
            expected: InputCategory::Shell,
        },
    ];

    println!("\n--- User Session Mode Switching Scenario ---");
    for case in &cases {
        let result = InputClassificationTask
            .run(
                &engine,
                InputClassificationInput {
                    user_input: case.input.to_string(),
                    recent_commands: case.recent.iter().map(|s| s.to_string()).collect(),
                    is_follow_up: case.follow_up,
                },
            )
            .await
            .expect("classification failed");

        println!(
            "  {:?} -> {:?} (expected {:?}, confidence {:.2})",
            case.input, result.category, case.expected, result.confidence
        );
        assert_eq!(
            result.category, case.expected,
            "Misclassified {:?}",
            case.input
        );
    }
}

/// Simulates prompt suggestions being generated after various shell commands.
#[tokio::test]
async fn scenario_prompt_suggestions_after_commands() {
    let engine = get_engine().await;

    println!("\n--- Prompt Suggestions After Commands Scenario ---");

    // After a failed build
    let suggestions = PromptSuggestionTask
        .run(
            &engine,
            PromptSuggestionInput {
                recent_commands: vec![
                    "cargo build".into(),
                    "cargo test -- --nocapture".into(),
                    "cargo build".into(), // repeated = likely still failing
                ],
                current_input: String::new(),
                working_directory: "/home/user/projects/rust-app".into(),
            },
        )
        .await
        .expect("suggestion failed");

    println!("  After repeated builds: {suggestions:?}");
    assert!(!suggestions.is_empty());

    // After git workflow
    let suggestions = PromptSuggestionTask
        .run(
            &engine,
            PromptSuggestionInput {
                recent_commands: vec![
                    "git add .".into(),
                    "git commit -m 'wip'".into(),
                    "git push".into(),
                ],
                current_input: String::new(),
                working_directory: "/home/user/projects/feature-branch".into(),
            },
        )
        .await
        .expect("suggestion failed");

    println!("  After git workflow: {suggestions:?}");
    assert!(!suggestions.is_empty());

    // With partial input
    let suggestions = PromptSuggestionTask
        .run(
            &engine,
            PromptSuggestionInput {
                recent_commands: vec!["docker ps".into(), "docker logs app".into()],
                current_input: "docker".to_string(),
                working_directory: "/home/user/deployments".into(),
            },
        )
        .await
        .expect("suggestion failed");

    println!("  With 'docker' partial input: {suggestions:?}");
    assert!(!suggestions.is_empty());
}

/// Simulates tab naming for different development contexts.
#[tokio::test]
async fn scenario_tab_naming_development_contexts() {
    let engine = get_engine().await;

    println!("\n--- Tab Naming for Development Contexts ---");

    struct TabCase {
        label: &'static str,
        commands: Vec<&'static str>,
        cwd: &'static str,
    }

    let cases = vec![
        TabCase {
            label: "Git operations",
            commands: vec!["git log --oneline", "git branch -a", "git fetch origin"],
            cwd: "/home/user/projects/galaxy",
        },
        TabCase {
            label: "Docker deployment",
            commands: vec!["docker compose up -d", "docker ps", "docker logs web"],
            cwd: "/home/user/services/api",
        },
        TabCase {
            label: "Python data science",
            commands: vec![
                "jupyter notebook",
                "pip install pandas",
                "python analysis.py",
            ],
            cwd: "/home/user/research/data-pipeline",
        },
        TabCase {
            label: "Rust development",
            commands: vec!["cargo build", "cargo test", "cargo clippy"],
            cwd: "/home/user/projects/my-crate",
        },
    ];

    for case in &cases {
        let name = TabNamingTask
            .run(
                &engine,
                TabNamingInput {
                    recent_commands: case.commands.iter().map(|s| s.to_string()).collect(),
                    working_directory: case.cwd.to_string(),
                },
            )
            .await
            .expect("tab naming failed");

        println!("  {}: \"{}\"", case.label, name);
        assert!(!name.is_empty(), "tab name should not be empty");
        assert!(
            name.split_whitespace().count() <= 6,
            "tab name too long for '{}': \"{}\"",
            case.label,
            name
        );
    }
}

/// Tests performance: all tasks should complete quickly enough for interactive use.
#[tokio::test]
async fn scenario_performance_under_load() {
    let engine = get_engine().await;

    println!("\n--- Performance Under Load ---");

    // Classification should be fast (< 500ms on CPU)
    let start = Instant::now();
    for _ in 0..5 {
        InputClassificationTask
            .run(
                &engine,
                InputClassificationInput {
                    user_input: "git push origin main".to_string(),
                    recent_commands: vec!["git add .".into(), "git commit -m 'test'".into()],
                    is_follow_up: false,
                },
            )
            .await
            .expect("classification failed");
    }
    let classification_time = start.elapsed();
    println!(
        "  5 classifications: {:?} (avg {:?})",
        classification_time,
        classification_time / 5
    );

    // Tab naming
    let start = Instant::now();
    TabNamingTask
        .run(
            &engine,
            TabNamingInput {
                recent_commands: vec!["make build".into(), "make test".into()],
                working_directory: "/home/user/project".into(),
            },
        )
        .await
        .expect("tab naming failed");
    let tab_time = start.elapsed();
    println!("  1 tab naming: {:?}", tab_time);

    // Prompt suggestion
    let start = Instant::now();
    PromptSuggestionTask
        .run(
            &engine,
            PromptSuggestionInput {
                recent_commands: vec!["ls".into(), "cd src".into()],
                current_input: String::new(),
                working_directory: "/home/user".into(),
            },
        )
        .await
        .expect("prompt suggestion failed");
    let suggest_time = start.elapsed();
    println!("  1 prompt suggestion: {:?}", suggest_time);

    // Classification should be under 2s per call on CPU (generous limit for CI)
    assert!(
        classification_time / 5 < std::time::Duration::from_secs(2),
        "classification too slow: {:?} per call",
        classification_time / 5
    );
}

/// Tests that the engine can be reused across many calls without degradation.
#[tokio::test]
async fn scenario_engine_stability_across_calls() {
    let engine = get_engine().await;

    println!("\n--- Engine Stability Across Calls ---");

    // Mix different task types in sequence
    for i in 0..3 {
        // Classification
        let result = InputClassificationTask
            .run(
                &engine,
                InputClassificationInput {
                    user_input: format!("test command {i}"),
                    recent_commands: vec![],
                    is_follow_up: false,
                },
            )
            .await
            .expect("classification should not degrade");
        assert!(result.confidence > 0.0);

        // Tab naming
        let name = TabNamingTask
            .run(
                &engine,
                TabNamingInput {
                    recent_commands: vec![format!("cmd {i}")],
                    working_directory: format!("/tmp/test-{i}"),
                },
            )
            .await
            .expect("tab naming should not degrade");
        assert!(!name.is_empty());

        // Prompt suggestion
        let suggestions = PromptSuggestionTask
            .run(
                &engine,
                PromptSuggestionInput {
                    recent_commands: vec![format!("action {i}")],
                    current_input: String::new(),
                    working_directory: "/tmp".into(),
                },
            )
            .await
            .expect("prompt suggestion should not degrade");
        assert!(!suggestions.is_empty());

        println!("  Round {}: all tasks passed", i + 1);
    }
}
