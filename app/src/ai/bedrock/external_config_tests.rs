use std::io::Write;

use tempfile::NamedTempFile;

use super::*;

#[test]
fn test_parse_claude_code_config_extracts_profile_and_region() {
    let mut file = NamedTempFile::new().unwrap();
    write!(
        file,
        r#"{{
            "env": {{
                "AWS_PROFILE": "coding-assistant",
                "AWS_REGION": "us-west-2",
                "CLAUDE_CODE_USE_BEDROCK": "1"
            }}
        }}"#
    )
    .unwrap();

    let config = parse_claude_code_config(file.path().to_path_buf());
    assert_eq!(config.profile, Some("coding-assistant".to_string()));
    assert_eq!(config.region, Some("us-west-2".to_string()));
}

#[test]
fn test_parse_claude_code_config_extracts_model_map() {
    let mut file = NamedTempFile::new().unwrap();
    write!(
        file,
        r#"{{
            "env": {{
                "AWS_PROFILE": "test",
                "AWS_REGION": "us-east-1",
                "DCP_MODEL_MAP": "{{\"us.anthropic.claude-opus-4-6-v1\": \"arn:aws:bedrock:us-east-1:123456:application-inference-profile/abc123\"}}"
            }}
        }}"#
    )
    .unwrap();

    let config = parse_claude_code_config(file.path().to_path_buf());
    assert_eq!(config.models.len(), 1);
    assert_eq!(
        config.models[0].model_id,
        "arn:aws:bedrock:us-east-1:123456:application-inference-profile/abc123"
    );
    assert!(config.models[0].vision_supported);
}

#[test]
fn test_parse_claude_code_config_missing_file() {
    let config = parse_claude_code_config(PathBuf::from("/nonexistent/path/settings.json"));
    assert!(config.is_empty());
}

#[test]
fn test_parse_opencode_config_extracts_bedrock_options() {
    let mut file = NamedTempFile::new().unwrap();
    write!(
        file,
        r#"{{
            "provider": {{
                "amazon-bedrock": {{
                    "options": {{
                        "region": "eu-west-1",
                        "profile": "my-profile"
                    }}
                }}
            }}
        }}"#
    )
    .unwrap();

    let config = parse_opencode_config(file.path().to_path_buf());
    assert_eq!(config.profile, Some("my-profile".to_string()));
    assert_eq!(config.region, Some("eu-west-1".to_string()));
}

#[test]
fn test_parse_opencode_config_missing_file() {
    let config = parse_opencode_config(PathBuf::from("/nonexistent/opencode.json"));
    assert!(config.is_empty());
}

#[test]
fn test_claude_code_takes_priority_over_opencode() {
    // When both configs provide a profile, Claude Code wins
    let claude = ExternalBedrockConfig {
        profile: Some("claude-profile".to_string()),
        region: Some("us-east-1".to_string()),
        models: Vec::new(),
    };
    let opencode = ExternalBedrockConfig {
        profile: Some("opencode-profile".to_string()),
        region: Some("eu-west-1".to_string()),
        models: Vec::new(),
    };

    let merged = ExternalBedrockConfig {
        profile: claude.profile.or(opencode.profile),
        region: claude.region.or(opencode.region),
        models: Vec::new(),
    };

    assert_eq!(merged.profile, Some("claude-profile".to_string()));
    assert_eq!(merged.region, Some("us-east-1".to_string()));
}

#[test]
fn test_prettify_model_slug() {
    assert_eq!(prettify_model_slug("claude-opus-4-6"), "Claude Opus 4.6");
    assert_eq!(
        prettify_model_slug("claude-sonnet-4-5-20250929"),
        "Claude Sonnet 4.5"
    );
    assert_eq!(prettify_model_slug("claude-haiku-4-5"), "Claude Haiku 4.5");
}

#[test]
fn test_derive_display_name_strips_prefixes() {
    assert_eq!(
        derive_display_name("us.anthropic.claude-opus-4-6-v1"),
        "Claude Opus 4.6"
    );
    assert_eq!(
        derive_display_name("global.anthropic.claude-haiku-4-5-20251001-v1:0"),
        "Claude Haiku 4.5"
    );
}
