#[cfg(test)]
mod prompt_builder_tests {
    use crate::ai::prompt_builder::*;

    #[test]
    fn test_code_mode_builds_successfully() {
        let prompt = PromptBuilder::new(Mode::Code, Provider::Anthropic)
            .with_context(
                PromptContext::new()
                    .with_working_dir("/home/user/project")
                    .with_os("Linux")
                    .with_shell("zsh 5.9"),
            )
            .build();

        assert!(prompt.system_prompt.contains("Galaxy"));
        assert!(prompt.system_prompt.contains("coding mode"));
        assert!(prompt.system_prompt.contains("/home/user/project"));
        assert!(prompt.system_prompt.contains("search_codebase` first"));
        assert!(!prompt.tools.is_empty());
        // Code mode should have apply_file_diffs
        assert!(prompt.tools.iter().any(|t| t.name == "apply_file_diffs"));
    }

    #[test]
    fn test_plan_mode_has_no_edit_tools() {
        let prompt = PromptBuilder::new(Mode::Plan, Provider::Anthropic).build();

        assert!(prompt.system_prompt.contains("planning mode"));
        assert!(prompt.system_prompt.contains("READ-ONLY"));
        // Plan mode should NOT have apply_file_diffs
        assert!(!prompt.tools.iter().any(|t| t.name == "apply_file_diffs"));
        // But should have read_files
        assert!(prompt.tools.iter().any(|t| t.name == "read_files"));
        assert!(prompt.system_prompt.contains("search_codebase` first"));
    }

    #[test]
    fn test_semantic_search_is_advertised_before_regex_search() {
        let prompt = PromptBuilder::new(Mode::Code, Provider::Anthropic).build();
        let search_index = prompt
            .tools
            .iter()
            .position(|tool| tool.name == "search_codebase")
            .expect("search_codebase tool");
        let grep_index = prompt
            .tools
            .iter()
            .position(|tool| tool.name == "grep")
            .expect("grep tool");
        assert!(search_index < grep_index);
        assert!(prompt
            .tools
            .iter()
            .find(|tool| tool.name == "search_codebase")
            .is_some_and(|tool| tool.description.contains("all source-code discovery")));
        assert!(prompt
            .tools
            .iter()
            .find(|tool| tool.name == "grep")
            .is_some_and(|tool| tool.description.contains("non-code files")));
    }

    #[test]
    fn test_review_mode_is_read_only() {
        let prompt = PromptBuilder::new(Mode::Review, Provider::Anthropic).build();

        assert!(prompt.system_prompt.contains("code review mode"));
        assert!(!prompt.tools.iter().any(|t| t.name == "apply_file_diffs"));
        assert!(!prompt.tools.iter().any(|t| t.name == "create_plan"));
        assert!(prompt.tools.iter().any(|t| t.name == "read_files"));
        assert!(prompt.tools.iter().any(|t| t.name == "grep"));
    }

    #[test]
    fn test_summarize_mode_has_no_tools() {
        let prompt = PromptBuilder::new(Mode::Summarize, Provider::Anthropic).build();

        assert!(prompt.system_prompt.contains("summariz"));
        assert!(prompt.tools.is_empty());
    }

    #[test]
    fn test_title_mode_has_no_tools() {
        let prompt = PromptBuilder::new(Mode::Title, Provider::Anthropic).build();

        assert!(prompt.system_prompt.contains("title"));
        assert!(prompt.tools.is_empty());
    }

    #[test]
    fn test_project_rules_included() {
        let prompt = PromptBuilder::new(Mode::Code, Provider::Anthropic)
            .with_project_rules(vec![ProjectRule {
                root_path: "/home/user/project".to_string(),
                content: "Always use snake_case for function names.".to_string(),
            }])
            .build();

        assert!(prompt.system_prompt.contains("Project Rules"));
        assert!(prompt.system_prompt.contains("snake_case"));
    }

    #[test]
    fn test_mcp_tools_appended() {
        use crate::ai::provider::convert::ToolDefinition;

        let mcp_tool = ToolDefinition {
            name: "mcp__github__create_pr".to_string(),
            description: "Create a pull request".to_string(),
            input_schema: serde_json::json!({"type": "object", "properties": {}}),
        };

        let prompt = PromptBuilder::new(Mode::Code, Provider::Anthropic)
            .with_mcp_tools(vec![mcp_tool.clone()])
            .build();

        assert!(prompt
            .tools
            .iter()
            .any(|t| t.name == "mcp__github__create_pr"));
    }

    #[test]
    fn test_mode_allows_edits() {
        assert!(Mode::Code.allows_edits());
        assert!(!Mode::Plan.allows_edits());
        assert!(!Mode::Review.allows_edits());
        assert!(!Mode::Summarize.allows_edits());
        assert!(!Mode::Title.allows_edits());
    }

    #[test]
    fn test_mode_allows_shell() {
        assert!(Mode::Code.allows_shell());
        assert!(!Mode::Plan.allows_shell());
        assert!(!Mode::Review.allows_shell());
    }

    #[test]
    fn test_empty_context_omits_environment_section() {
        let prompt = PromptBuilder::new(Mode::Code, Provider::Anthropic).build();

        // With default (empty) context, no Environment section
        assert!(!prompt.system_prompt.contains("## Environment"));
    }

    #[test]
    fn test_context_populates_environment_section() {
        let prompt = PromptBuilder::new(Mode::Code, Provider::Anthropic)
            .with_context(
                PromptContext::new()
                    .with_working_dir("/projects/myapp")
                    .with_git_branch("feature/new-thing"),
            )
            .build();

        assert!(prompt.system_prompt.contains("## Environment"));
        assert!(prompt.system_prompt.contains("/projects/myapp"));
        assert!(prompt.system_prompt.contains("feature/new-thing"));
    }
}
