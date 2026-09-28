use std::str::FromStr;

use settings_page::MatchData;

use super::nav::{SettingsNavItem, SettingsUmbrella};
use super::*;

#[test]
fn ai_subpages_are_classified_and_map_to_their_backing_pages() {
    for section in SettingsSection::ai_subpages() {
        assert!(
            section.is_ai_subpage(),
            "{section:?} should be an AI subpage"
        );
        assert!(section.is_subpage(), "{section:?} should be a subpage");

        let expected_parent = if *section == SettingsSection::AgentMCPServers {
            SettingsSection::MCPServers
        } else {
            SettingsSection::AI
        };
        assert_eq!(section.parent_page_section(), expected_parent);
    }

    for section in [
        SettingsSection::AI,
        SettingsSection::Appearance,
        SettingsSection::Code,
        SettingsSection::Privacy,
        SettingsSection::Scripting,
    ] {
        assert!(!section.is_ai_subpage(), "{section:?} is a top-level page");
    }
}

#[test]
fn provider_subpages_are_classified_and_map_to_ai_backing_page() {
    assert_eq!(
        SettingsSection::provider_subpages(),
        &[
            SettingsSection::ProviderOpenAI,
            SettingsSection::ProviderLiteLLM,
            SettingsSection::ProviderChatGPTSubscription,
            SettingsSection::ProviderBedrock,
            SettingsSection::ProviderACP,
        ]
    );
    for section in SettingsSection::provider_subpages() {
        assert!(section.is_provider_subpage());
        assert!(section.is_subpage());
        assert_eq!(
            section.parent_page_section(),
            SettingsSection::AI,
            "{section:?} should use the AI backing page"
        );
    }
    assert!(!SettingsSection::AI.is_provider_subpage());
}

#[test]
fn code_subpages_are_classified_and_map_to_code() {
    assert_eq!(
        SettingsSection::code_subpages(),
        &[
            SettingsSection::CodeIndexing,
            SettingsSection::EditorAndCodeReview,
        ]
    );
    for section in SettingsSection::code_subpages() {
        assert!(section.is_code_subpage());
        assert!(section.is_subpage());
        assert_eq!(
            section.parent_page_section(),
            SettingsSection::Code,
            "{section:?} should use the Code backing page"
        );
    }
    assert!(!SettingsSection::Code.is_code_subpage());
}

#[test]
fn top_level_sections_map_to_themselves() {
    for section in [
        SettingsSection::About,
        SettingsSection::Appearance,
        SettingsSection::Features,
        SettingsSection::Keybindings,
        SettingsSection::Privacy,
        SettingsSection::Scripting,
        SettingsSection::Teams,
        SettingsSection::WarpDrive,
        SettingsSection::Wormhole,
    ] {
        assert!(!section.is_subpage(), "{section:?} should be top-level");
        assert_eq!(section.parent_page_section(), section);
    }
}

#[test]
fn current_settings_display_names_round_trip() {
    for (section, display_name) in [
        (SettingsSection::Scripting, "Galaxy Control"),
        (SettingsSection::WarpDrive, "Galaxy Drive"),
        (SettingsSection::Wormhole, "Wormhole"),
        (SettingsSection::WarpAgent, "Galaxy Agent"),
        (SettingsSection::AgentProfiles, "Profiles"),
        (SettingsSection::AgentMCPServers, "MCP servers"),
        (SettingsSection::Knowledge, "Knowledge"),
        (
            SettingsSection::ThirdPartyCLIAgents,
            "Third party CLI agents",
        ),
        (SettingsSection::ProviderOpenAI, "OpenAI"),
        (SettingsSection::ProviderLiteLLM, "LiteLLM"),
        (
            SettingsSection::ProviderChatGPTSubscription,
            "ChatGPT Subscription",
        ),
        (SettingsSection::ProviderBedrock, "Bedrock"),
        (SettingsSection::ProviderACP, "ACP"),
        (SettingsSection::CodeIndexing, "Indexing and projects"),
        (
            SettingsSection::EditorAndCodeReview,
            "Editor and Code Review",
        ),
    ] {
        assert_eq!(section.to_string(), display_name);
        assert_eq!(SettingsSection::from_str(display_name), Ok(section));
    }
}

#[test]
fn legacy_settings_names_remain_parseable() {
    for (name, expected) in [
        ("Scripting", SettingsSection::Scripting),
        ("WarpDrive", SettingsSection::WarpDrive),
        ("Wormhole", SettingsSection::Wormhole),
        ("Oz", SettingsSection::WarpAgent),
        ("Warp Agent", SettingsSection::WarpAgent),
        ("AgentProfiles", SettingsSection::AgentProfiles),
        ("AgentMCPServers", SettingsSection::AgentMCPServers),
        ("ThirdPartyCLIAgents", SettingsSection::ThirdPartyCLIAgents),
        ("AWS Bedrock", SettingsSection::ProviderBedrock),
        ("Bedrock", SettingsSection::ProviderBedrock),
        ("OpenAI / LiteLLM", SettingsSection::ProviderLiteLLM),
        ("LiteLLM", SettingsSection::ProviderLiteLLM),
        ("OpenAI", SettingsSection::ProviderOpenAI),
        ("Models", SettingsSection::ProviderOpenAI),
        ("Agent runtimes", SettingsSection::ProviderOpenAI),
        (
            "ChatGPTSubscription",
            SettingsSection::ProviderChatGPTSubscription,
        ),
        (
            "ChatGPT Subscription",
            SettingsSection::ProviderChatGPTSubscription,
        ),
        ("Agent Client Protocol", SettingsSection::ProviderACP),
        ("ACP", SettingsSection::ProviderACP),
        ("CodeIndexing", SettingsSection::CodeIndexing),
        ("EditorAndCodeReview", SettingsSection::EditorAndCodeReview),
    ] {
        assert_eq!(SettingsSection::from_str(name), Ok(expected), "{name}");
    }
    assert_eq!(SettingsSection::from_str("not-a-section"), Err(()));
}

#[test]
fn match_data_truthiness_tracks_visibility() {
    assert!(MatchData::Uncounted(true).is_truthy());
    assert!(!MatchData::Uncounted(false).is_truthy());
    assert!(MatchData::Countable(1).is_truthy());
    assert!(!MatchData::Countable(0).is_truthy());
}

fn realistic_nav_items() -> Vec<SettingsNavItem> {
    vec![
        SettingsNavItem::Umbrella(SettingsUmbrella::new(
            "Agents",
            SettingsSection::ai_subpages().to_vec(),
        )),
        SettingsNavItem::Umbrella(SettingsUmbrella::new(
            "Providers",
            SettingsSection::provider_subpages().to_vec(),
        )),
        SettingsNavItem::Umbrella(SettingsUmbrella::new(
            "Code",
            SettingsSection::code_subpages().to_vec(),
        )),
        SettingsNavItem::Page(SettingsSection::Appearance),
        SettingsNavItem::Page(SettingsSection::Features),
        SettingsNavItem::Page(SettingsSection::Keybindings),
        SettingsNavItem::Page(SettingsSection::Wormhole),
        SettingsNavItem::Page(SettingsSection::WarpDrive),
        SettingsNavItem::Page(SettingsSection::Privacy),
        SettingsNavItem::Page(SettingsSection::About),
        SettingsNavItem::Page(SettingsSection::Scripting),
    ]
}

fn set_expanded(nav_items: &mut [SettingsNavItem], nav_index: usize, expanded: bool) {
    let Some(SettingsNavItem::Umbrella(umbrella)) = nav_items.get_mut(nav_index) else {
        panic!("nav_items[{nav_index}] is not an umbrella");
    };
    umbrella.expanded = expanded;
}

#[test]
fn collapsed_umbrellas_each_form_one_navigation_stop() {
    let nav_items = realistic_nav_items();
    let stops = build_nav_stops(&nav_items, |_| true);

    assert_eq!(stops.len(), 11);
    assert_eq!(
        stops[0],
        NavStop::CollapsedUmbrella {
            nav_index: 0,
            first_subpage: SettingsSection::WarpAgent,
            last_subpage: SettingsSection::ThirdPartyCLIAgents,
        }
    );
    assert_eq!(
        stops[1],
        NavStop::CollapsedUmbrella {
            nav_index: 1,
            first_subpage: SettingsSection::ProviderOpenAI,
            last_subpage: SettingsSection::ProviderACP,
        }
    );
    assert_eq!(
        stops[2],
        NavStop::CollapsedUmbrella {
            nav_index: 2,
            first_subpage: SettingsSection::CodeIndexing,
            last_subpage: SettingsSection::EditorAndCodeReview,
        }
    );
    assert_eq!(stops[3], NavStop::Section(SettingsSection::Appearance));
    assert_eq!(stops[10], NavStop::Section(SettingsSection::Scripting));
}

#[test]
fn expanded_umbrella_has_one_stop_per_visible_subpage() {
    let mut nav_items = realistic_nav_items();
    set_expanded(&mut nav_items, 0, true);

    let stops = build_nav_stops(&nav_items, |_| true);
    let expected_ai_sections = SettingsSection::ai_subpages()
        .iter()
        .copied()
        .map(NavStop::Section)
        .collect::<Vec<_>>();

    assert_eq!(
        &stops[..expected_ai_sections.len()],
        expected_ai_sections.as_slice()
    );
    assert_eq!(
        stops[expected_ai_sections.len()],
        NavStop::CollapsedUmbrella {
            nav_index: 1,
            first_subpage: SettingsSection::ProviderOpenAI,
            last_subpage: SettingsSection::ProviderACP,
        }
    );
}

#[test]
fn collapsed_umbrella_uses_first_and_last_visible_subpages() {
    let nav_items = realistic_nav_items();
    let stops = build_nav_stops(&nav_items, |section| {
        !matches!(
            section,
            SettingsSection::WarpAgent | SettingsSection::Models
        )
    });

    assert_eq!(
        stops[0],
        NavStop::CollapsedUmbrella {
            nav_index: 0,
            first_subpage: SettingsSection::AgentProfiles,
            last_subpage: SettingsSection::ThirdPartyCLIAgents,
        }
    );
}

#[test]
fn umbrella_without_visible_subpages_is_skipped() {
    let nav_items = realistic_nav_items();
    let stops = build_nav_stops(&nav_items, |section| !section.is_ai_subpage());

    assert!(stops
        .iter()
        .all(|stop| !matches!(stop, NavStop::CollapsedUmbrella { nav_index: 0, .. })));
    assert!(stops
        .iter()
        .any(|stop| matches!(stop, NavStop::CollapsedUmbrella { nav_index: 1, .. })));
    assert!(stops
        .iter()
        .any(|stop| matches!(stop, NavStop::CollapsedUmbrella { nav_index: 2, .. })));
}

#[test]
fn filtered_top_level_page_is_skipped() {
    let nav_items = realistic_nav_items();
    let stops = build_nav_stops(&nav_items, |section| section != SettingsSection::Scripting);

    assert!(stops
        .iter()
        .all(|stop| *stop != NavStop::Section(SettingsSection::Scripting)));
}

#[test]
fn current_stop_matches_sections_and_collapsed_umbrella_children() {
    let nav_items = realistic_nav_items();
    let stops = build_nav_stops(&nav_items, |_| true);

    assert_eq!(
        current_stop_index(&stops, &nav_items, SettingsSection::Appearance),
        Some(3)
    );
    assert_eq!(
        current_stop_index(&stops, &nav_items, SettingsSection::Knowledge),
        Some(0)
    );
    assert_eq!(
        current_stop_index(&stops, &nav_items, SettingsSection::ProviderLiteLLM),
        Some(1)
    );
    assert_eq!(
        current_stop_index(&stops, &nav_items, SettingsSection::CodeIndexing),
        Some(2)
    );
}

#[test]
fn current_stop_returns_none_for_filtered_section() {
    let nav_items = realistic_nav_items();
    let stops = build_nav_stops(&nav_items, |section| !section.is_ai_subpage());

    assert_eq!(
        current_stop_index(&stops, &nav_items, SettingsSection::Knowledge),
        None
    );
}

#[test]
fn next_stop_wraps_in_both_directions() {
    assert_eq!(next_stop_index(0, 3, CycleDirection::Up), 2);
    assert_eq!(next_stop_index(2, 3, CycleDirection::Down), 0);
    assert_eq!(next_stop_index(1, 3, CycleDirection::Up), 0);
    assert_eq!(next_stop_index(1, 3, CycleDirection::Down), 2);
    assert_eq!(next_stop_index(0, 1, CycleDirection::Down), 0);
}

fn simulate_cycle(
    nav_items: &[SettingsNavItem],
    stops: &[NavStop],
    current: SettingsSection,
    direction: CycleDirection,
) -> SettingsSection {
    let active = current_stop_index(stops, nav_items, current)
        .expect("current section should have a navigation stop");
    let next = next_stop_index(active, stops.len(), direction);
    match stops[next] {
        NavStop::Section(section) => section,
        NavStop::CollapsedUmbrella {
            first_subpage,
            last_subpage,
            ..
        } => match direction {
            CycleDirection::Up => last_subpage,
            CycleDirection::Down => first_subpage,
        },
    }
}

#[test]
fn cycling_enters_collapsed_umbrellas_in_reading_order() {
    let nav_items = realistic_nav_items();
    let stops = build_nav_stops(&nav_items, |_| true);

    assert_eq!(
        simulate_cycle(
            &nav_items,
            &stops,
            SettingsSection::Scripting,
            CycleDirection::Down,
        ),
        SettingsSection::WarpAgent
    );
    assert_eq!(
        simulate_cycle(
            &nav_items,
            &stops,
            SettingsSection::Appearance,
            CycleDirection::Up,
        ),
        SettingsSection::EditorAndCodeReview
    );
}

#[test]
fn cycling_leaves_an_expanded_umbrella_after_its_last_subpage() {
    let mut nav_items = realistic_nav_items();
    set_expanded(&mut nav_items, 0, true);
    let stops = build_nav_stops(&nav_items, |_| true);

    assert_eq!(
        simulate_cycle(
            &nav_items,
            &stops,
            SettingsSection::ThirdPartyCLIAgents,
            CycleDirection::Down,
        ),
        SettingsSection::ProviderOpenAI
    );
}
