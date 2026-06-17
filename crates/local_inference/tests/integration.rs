use local_inference::{
    Device, GenerationConfig, InferenceEngine, InferenceTask, InputCategory,
    InputClassificationInput, InputClassificationTask, PromptSuggestionInput, PromptSuggestionTask,
    TabNamingInput, TabNamingTask,
};

async fn get_engine() -> InferenceEngine {
    InferenceEngine::new(Device::Cpu)
        .await
        .expect("Failed to initialize engine — is the model downloaded?")
}

#[tokio::test]
async fn test_engine_initializes() {
    let engine = get_engine().await;
    assert!(engine.tokenizer().get_vocab_size(true) > 0);
}

#[tokio::test]
async fn test_basic_generation() {
    let engine = get_engine().await;
    let config = GenerationConfig::deterministic();
    let output = engine
        .generate("The capital of France is", &config)
        .await
        .expect("generation failed");
    assert!(!output.is_empty(), "generated output should not be empty");
    println!("Generated: {output}");
}

#[tokio::test]
async fn test_classify_shell_command() {
    let engine = get_engine().await;
    let result = InputClassificationTask
        .run(
            &engine,
            InputClassificationInput {
                user_input: "ls -la /tmp".to_string(),
                recent_commands: vec!["cd /tmp".into(), "mkdir test".into()],
                is_follow_up: false,
            },
        )
        .await
        .expect("classification failed");

    println!(
        "\"ls -la /tmp\" -> {:?} (confidence: {:.2})",
        result.category, result.confidence
    );
    assert_eq!(result.category, InputCategory::Shell);
}

#[tokio::test]
async fn test_classify_agent_prompt() {
    let engine = get_engine().await;
    let result = InputClassificationTask
        .run(
            &engine,
            InputClassificationInput {
                user_input: "explain how async works in rust".to_string(),
                recent_commands: vec!["cargo build".into()],
                is_follow_up: false,
            },
        )
        .await
        .expect("classification failed");

    println!(
        "\"explain how async works in rust\" -> {:?} (confidence: {:.2})",
        result.category, result.confidence
    );
    assert_eq!(result.category, InputCategory::AgentPrompt);
}

#[tokio::test]
async fn test_classify_ambiguous_input() {
    let engine = get_engine().await;

    // "docker" alone could be either — we just want to make sure it doesn't panic
    let result = InputClassificationTask
        .run(
            &engine,
            InputClassificationInput {
                user_input: "docker".to_string(),
                recent_commands: vec![],
                is_follow_up: false,
            },
        )
        .await
        .expect("classification failed");

    println!(
        "\"docker\" -> {:?} (confidence: {:.2})",
        result.category, result.confidence
    );
    // Either classification is fine, just assert it doesn't crash
    assert!(result.confidence > 0.0);
}

#[tokio::test]
async fn test_tab_naming() {
    let engine = get_engine().await;
    let name = TabNamingTask
        .run(
            &engine,
            TabNamingInput {
                recent_commands: vec![
                    "git log --oneline".into(),
                    "git diff".into(),
                    "git add .".into(),
                ],
                working_directory: "/home/user/projects/galaxy".into(),
            },
        )
        .await
        .expect("tab naming failed");

    println!("Tab name: \"{name}\"");
    assert!(!name.is_empty());
    // Tab names should be short
    assert!(
        name.split_whitespace().count() <= 6,
        "tab name too long: \"{name}\""
    );
}

#[tokio::test]
async fn test_prompt_suggestions() {
    let engine = get_engine().await;
    let suggestions = PromptSuggestionTask
        .run(
            &engine,
            PromptSuggestionInput {
                recent_commands: vec!["cargo build".into(), "cargo test".into()],
                current_input: "cargo".to_string(),
                working_directory: "/home/user/projects/galaxy".into(),
            },
        )
        .await
        .expect("prompt suggestion failed");

    println!("Suggestions: {suggestions:?}");
    assert!(
        !suggestions.is_empty(),
        "should have at least one suggestion"
    );
}

#[tokio::test]
async fn test_generation_respects_max_tokens() {
    let engine = get_engine().await;
    let config = GenerationConfig {
        max_tokens: 5,
        temperature: 0.0,
        top_p: 1.0,
    };

    let output = engine
        .generate("Once upon a time", &config)
        .await
        .expect("generation failed");

    let token_count = engine
        .tokenizer()
        .encode(output.as_str(), false)
        .map(|e| e.get_ids().len())
        .unwrap_or(0);

    println!("Generated ({token_count} tokens): \"{output}\"");
    // Should be roughly around max_tokens (could be less if EOS hit)
    assert!(token_count <= 6, "generated too many tokens: {token_count}");
}

// --- Edge case tests ---

#[tokio::test]
async fn test_classify_empty_input() {
    let engine = get_engine().await;
    let result = InputClassificationTask
        .run(
            &engine,
            InputClassificationInput {
                user_input: String::new(),
                recent_commands: vec![],
                is_follow_up: false,
            },
        )
        .await
        .expect("classification should not panic on empty input");

    println!(
        "empty input -> {:?} (confidence: {:.2})",
        result.category, result.confidence
    );
    assert!(result.confidence > 0.0);
}

#[tokio::test]
async fn test_classify_very_long_input() {
    let engine = get_engine().await;
    let long_input = "a".repeat(500);
    let result = InputClassificationTask
        .run(
            &engine,
            InputClassificationInput {
                user_input: long_input,
                recent_commands: vec!["ls".into()],
                is_follow_up: false,
            },
        )
        .await
        .expect("classification should handle long input");

    println!(
        "long input -> {:?} (confidence: {:.2})",
        result.category, result.confidence
    );
    assert!(result.confidence > 0.0);
}

#[tokio::test]
async fn test_classify_special_characters() {
    let engine = get_engine().await;
    let result = InputClassificationTask
        .run(
            &engine,
            InputClassificationInput {
                user_input: "find . -name '*.rs' | xargs grep -l 'TODO'".to_string(),
                recent_commands: vec!["grep -rn test .".into()],
                is_follow_up: false,
            },
        )
        .await
        .expect("classification should handle special characters");

    println!(
        "pipe command -> {:?} (confidence: {:.2})",
        result.category, result.confidence
    );
    assert_eq!(result.category, InputCategory::Shell);
}

#[tokio::test]
async fn test_classify_follow_up_context() {
    let engine = get_engine().await;
    let result = InputClassificationTask
        .run(
            &engine,
            InputClassificationInput {
                user_input: "can you also add error handling?".to_string(),
                recent_commands: vec!["cargo build".into()],
                is_follow_up: true,
            },
        )
        .await
        .expect("classification should handle follow-up");

    println!(
        "follow-up -> {:?} (confidence: {:.2})",
        result.category, result.confidence
    );
    assert_eq!(result.category, InputCategory::AgentPrompt);
}

#[tokio::test]
async fn test_classify_multiple_sequential_calls() {
    let engine = get_engine().await;

    let inputs = vec![
        ("git status", InputCategory::Shell),
        ("what does this error mean?", InputCategory::AgentPrompt),
        ("npm install express", InputCategory::Shell),
        (
            "refactor this to use async/await",
            InputCategory::AgentPrompt,
        ),
    ];

    for (input, expected_category) in inputs {
        let result = InputClassificationTask
            .run(
                &engine,
                InputClassificationInput {
                    user_input: input.to_string(),
                    recent_commands: vec!["ls".into()],
                    is_follow_up: false,
                },
            )
            .await
            .expect("classification failed");

        println!(
            "\"{input}\" -> {:?} (confidence: {:.2})",
            result.category, result.confidence
        );
        assert_eq!(
            result.category, expected_category,
            "expected {expected_category:?} for \"{input}\", got {:?}",
            result.category
        );
    }
}

#[tokio::test]
async fn test_tab_naming_empty_commands() {
    let engine = get_engine().await;
    let name = TabNamingTask
        .run(
            &engine,
            TabNamingInput {
                recent_commands: vec![],
                working_directory: "/home/user".into(),
            },
        )
        .await
        .expect("tab naming should handle empty commands");

    println!("Tab name (no commands): \"{name}\"");
    assert!(!name.is_empty());
}

#[tokio::test]
async fn test_tab_naming_deep_directory() {
    let engine = get_engine().await;
    let name = TabNamingTask
        .run(
            &engine,
            TabNamingInput {
                recent_commands: vec!["python train.py".into(), "tensorboard --logdir=runs".into()],
                working_directory: "/home/user/projects/ml-research/experiments/transformer-v2"
                    .into(),
            },
        )
        .await
        .expect("tab naming failed");

    println!("Tab name (deep dir): \"{name}\"");
    assert!(!name.is_empty());
    assert!(
        name.split_whitespace().count() <= 6,
        "tab name too long: \"{name}\""
    );
}

#[tokio::test]
async fn test_prompt_suggestions_empty_input() {
    let engine = get_engine().await;
    let suggestions = PromptSuggestionTask
        .run(
            &engine,
            PromptSuggestionInput {
                recent_commands: vec!["git status".into(), "git add .".into()],
                current_input: String::new(),
                working_directory: "/home/user/project".into(),
            },
        )
        .await
        .expect("prompt suggestion should handle empty input");

    println!("Suggestions (empty input): {suggestions:?}");
    assert!(
        !suggestions.is_empty(),
        "should suggest something even with empty input"
    );
}

#[tokio::test]
async fn test_prompt_suggestions_no_history() {
    let engine = get_engine().await;
    let suggestions = PromptSuggestionTask
        .run(
            &engine,
            PromptSuggestionInput {
                recent_commands: vec![],
                current_input: "docker".to_string(),
                working_directory: "/tmp".into(),
            },
        )
        .await
        .expect("prompt suggestion should handle no history");

    println!("Suggestions (no history): {suggestions:?}");
    assert!(
        !suggestions.is_empty(),
        "should suggest something even without history"
    );
}

#[tokio::test]
async fn test_deterministic_classification() {
    let engine = get_engine().await;
    let input = InputClassificationInput {
        user_input: "ls -la".to_string(),
        recent_commands: vec!["cd /tmp".into()],
        is_follow_up: false,
    };

    let result1 = InputClassificationTask
        .run(&engine, input.clone())
        .await
        .expect("first classification failed");
    let result2 = InputClassificationTask
        .run(&engine, input)
        .await
        .expect("second classification failed");

    // With temperature 0.0, results should be deterministic
    assert_eq!(
        result1.category, result2.category,
        "deterministic classification should yield consistent results"
    );
}
