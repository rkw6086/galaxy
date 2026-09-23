use galaxy_completer::completer::testing::FakeCompletionContext;
use galaxy_completer::signatures::testing::create_test_command_registry;
use galaxy_completer::ParsedTokensSnapshot;
use input_classifier::InputType;

use super::detect_input_type;

#[test]
fn detection_uses_command_inventory_and_history_without_running_input() {
    let context = FakeCompletionContext::new(create_test_command_registry([]))
        .with_top_level_commands(["git", "yarn"])
        .with_aliases([("gs".into(), "git status".to_string())].into())
        .with_functions(["deploy_app".into()].into())
        .with_builtins(["cd".into()].into());
    let history = vec!["custom_tool --old-argument".to_string()];
    for command in [
        "git pull origin CI",
        "yarn workspace app tsc",
        "gs",
        "deploy_app",
        "cd /tmp",
        "custom_tool --new-argument",
        "FOO=bar git status",
        "./script/check",
    ] {
        let input = ParsedTokensSnapshot {
            buffer_text: command.to_string(),
            parsed_tokens: vec![],
        };
        assert_eq!(
            detect_input_type(&input, &context, &history, InputType::AI),
            InputType::Shell,
            "{command}"
        );
    }
    for prompt in ["hello, there!", "please fix the build", "gti status"] {
        let input = ParsedTokensSnapshot {
            buffer_text: prompt.to_string(),
            parsed_tokens: vec![],
        };
        assert_eq!(
            detect_input_type(&input, &context, &history, InputType::Shell),
            InputType::AI,
            "{prompt}"
        );
    }
}

#[test]
fn command_prefix_preserves_current_mode() {
    let context = FakeCompletionContext::new(create_test_command_registry([]))
        .with_top_level_commands(["git"]);
    let input = ParsedTokensSnapshot {
        buffer_text: "gi".to_string(),
        parsed_tokens: vec![],
    };
    for current in [InputType::AI, InputType::Shell] {
        assert_eq!(detect_input_type(&input, &context, &[], current), current);
    }
}

#[test]
fn unverified_greeting_history_does_not_turn_a_prompt_into_a_command() {
    let context = FakeCompletionContext::new(create_test_command_registry([]));
    let records = [
        ("Hello, there!", None),
        ("Hello, there!", Some(127.into())),
        ("Hello, there!", Some(1.into())),
        ("Hello, there!", Some(130.into())),
        ("custom_tool --previous", Some(0.into())),
    ];
    let history = records
        .into_iter()
        .filter(|(_, exit_code)| super::is_verified_history_command(*exit_code))
        .map(|(command, _)| command.to_string())
        .collect::<Vec<_>>();
    assert_eq!(history, vec!["custom_tool --previous"]);
    let greeting = ParsedTokensSnapshot {
        buffer_text: "Hello, there!".to_string(),
        parsed_tokens: vec![],
    };
    assert_eq!(
        detect_input_type(&greeting, &context, &history, InputType::Shell),
        InputType::AI
    );
}
