use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use galaxy_core::ui::Icon;
use galaxyui::elements::{
    ConstrainedBox, Container, CrossAxisAlignment, Flex, ParentElement, SavePosition, Shrinkable,
    Text,
};
use galaxyui::fonts::{Properties, Style};
use galaxyui::{Action, AppContext, Element, SingletonEntity as _};
use itertools::Itertools;

use crate::ai::custom_model_routers::is_custom_router_id;
use crate::ai::llms::{
    is_using_api_key_for_provider, should_show_bedrock_icon_for_model, DisableReason, LLMId,
    LLMInfo, LLMPreferences,
};
use crate::menu::{MenuItem, MenuItemFields, MenuTooltipPosition};

pub fn is_auto(llm: &LLMInfo) -> bool {
    llm.display_name.to_lowercase().contains("auto")
        || llm.id.to_string().to_lowercase().contains("auto")
}

/// Returns true when a model's configured connection advertises reasoning choices.
pub fn has_reasoning_variants(llm: &LLMInfo, all_models: &[&LLMInfo]) -> bool {
    let key = llm.reasoning_group_key();
    all_models
        .iter()
        .any(|other| other.has_reasoning_level() && other.reasoning_group_key() == key)
}

/// Keep one visible model entry per connection while preserving the selected
/// reasoning variant as the entry's action. Unknown reasoning metadata stays unchanged.
pub fn collapse_model_reasoning_choices<'a>(
    choices: Vec<&'a LLMInfo>,
    selected_id: Option<&LLMId>,
) -> Vec<&'a LLMInfo> {
    struct ReasoningGroup {
        first_index: usize,
        default_index: Option<usize>,
        selected_index: Option<usize>,
        has_variants: bool,
        emitted: bool,
    }

    let mut group_by_key = HashMap::new();
    let mut groups = Vec::new();
    let mut group_indices = Vec::with_capacity(choices.len());
    for (index, model) in choices.iter().enumerate() {
        let group_index = *group_by_key
            .entry(model.reasoning_group_key())
            .or_insert_with(|| {
                groups.push(ReasoningGroup {
                    first_index: index,
                    default_index: None,
                    selected_index: None,
                    has_variants: false,
                    emitted: false,
                });
                groups.len() - 1
            });
        let group = &mut groups[group_index];
        if model.has_reasoning_level() {
            group.has_variants = true;
        } else {
            group.default_index.get_or_insert(index);
        }
        if Some(&model.id) == selected_id {
            group.selected_index.get_or_insert(index);
        }
        group_indices.push(group_index);
    }

    let mut result = Vec::with_capacity(choices.len());
    for (model, group_index) in choices.iter().zip(group_indices) {
        let group = &mut groups[group_index];
        if !group.has_variants {
            result.push(*model);
        } else if !group.emitted {
            group.emitted = true;
            let representative = group
                .selected_index
                .or(group.default_index)
                .unwrap_or(group.first_index);
            result.push(choices[representative]);
        }
    }
    result
}

/// Includes the provider-default entry only when that entry is actually available.
pub fn reasoning_choices_for_model<'a>(
    selected_id: &LLMId,
    choices: &[&'a LLMInfo],
) -> Vec<&'a LLMInfo> {
    let Some(selected) = choices.iter().find(|model| &model.id == selected_id) else {
        return Vec::new();
    };
    let key = selected.reasoning_group_key();
    let mut levels = HashSet::new();
    let mut result = Vec::new();
    let mut has_variants = false;
    for model in choices.iter().copied() {
        if model.reasoning_group_key() != key {
            continue;
        }
        has_variants |= model.has_reasoning_level();
        if levels.insert(model.reasoning_level.clone()) {
            result.push(model);
        }
    }
    if has_variants {
        result
    } else {
        Vec::new()
    }
}

fn with_cost_and_profile_info<A: Action + Clone>(
    item: MenuItemFields<A>,
    llm: &LLMInfo,
    profile_default_model: Option<&LLMId>,
) -> MenuItemFields<A> {
    let mut label = String::new();

    if Some(&llm.id) == profile_default_model {
        label.push_str("Profile default");
    }

    match llm.usage_metadata.credit_multiplier {
        Some(mult) if mult != 1. => {
            let mut formatted_cost = format!("~{mult:.1}")
                .trim_end_matches('0')
                .trim_end_matches('.')
                .to_string();
            formatted_cost.push('x');
            if label.is_empty() {
                label.push_str(&formatted_cost);
            } else {
                label.push_str(&format!(" ({formatted_cost})"));
            }
        }
        _ => {}
    }

    if label.is_empty() {
        item
    } else {
        // Using the key shortcut label to display extra info is a hack.
        item.with_key_shortcut_label(Some(label))
    }
}

fn make_item_fields<A: Action + Clone>(
    llm: &LLMInfo,
    action: impl Fn(&LLMInfo) -> A,
    position_id_fn: Option<&dyn Fn(&LLMId) -> String>,
    model_id_to_add_profile_default_label_to: Option<&LLMId>,
    collapse_auto: bool,
    collapse_reasoning_variants: bool,
    app: &AppContext,
) -> MenuItem<A> {
    let label = if collapse_auto && is_auto(llm) {
        llm.label_with_provider("auto")
    } else if collapse_reasoning_variants && llm.has_reasoning_level() {
        llm.label_with_provider(llm.base_model_name())
    } else {
        llm.menu_display_name()
    };
    let is_custom_endpoint = LLMPreferences::as_ref(app)
        .custom_llm_info_for_id(&llm.id)
        .is_some();
    let is_using_bedrock = should_show_bedrock_icon_for_model(llm, app);
    let is_using_api_key = is_custom_endpoint || is_using_api_key_for_provider(&llm.provider, app);
    let is_custom_router = is_custom_router_id(llm.id.as_str());
    let leading_icon = if is_using_bedrock {
        Icon::Aws
    } else if is_custom_router {
        Icon::Dataflow
    } else if matches!(llm.provider, crate::ai::llms::LLMProvider::Acp) {
        let model_id = llm.id.as_str().to_ascii_lowercase();
        if model_id.contains("claude") {
            Icon::ClaudeLogo
        } else if model_id.contains("gemini") {
            Icon::GeminiLogo
        } else if model_id.contains("codex") {
            Icon::OpenAILogo
        } else {
            Icon::Terminal
        }
    } else {
        llm.provider.icon().unwrap_or(Icon::Oz)
    };
    let trailing_credential_icon = if !is_using_bedrock && is_using_api_key {
        Some(Icon::Key)
    } else {
        None
    };

    let mut item = if let Some(position_id_fn) = position_id_fn {
        let position_id = position_id_fn(&llm.id);
        MenuItemFields::new_with_custom_label(
            Arc::new(move |_, _, appearance, _| {
                let mut item_row =
                    Flex::row().with_cross_axis_alignment(CrossAxisAlignment::Center);

                let icon_container = Container::new(
                    ConstrainedBox::new(
                        leading_icon
                            .to_galaxyui_icon(appearance.theme().foreground())
                            .finish(),
                    )
                    .with_height(appearance.ui_font_size())
                    .with_width(appearance.ui_font_size())
                    .finish(),
                )
                .with_margin_right(appearance.ui_font_size() / 2.)
                .finish();
                item_row.add_child(icon_container);

                let text = Text::new(
                    label.clone(),
                    appearance.ui_font_family(),
                    appearance.ui_font_size(),
                )
                .with_color(
                    appearance
                        .theme()
                        .main_text_color(appearance.theme().background())
                        .into(),
                )
                .finish();
                item_row.add_child(Shrinkable::new(4., text).finish());
                if let Some(icon) = trailing_credential_icon {
                    let credential_icon = Container::new(
                        ConstrainedBox::new(
                            icon.to_warpui_icon(appearance.theme().disabled_ui_text_color())
                                .finish(),
                        )
                        .with_height(appearance.ui_font_size())
                        .with_width(appearance.ui_font_size())
                        .finish(),
                    )
                    .with_margin_left(6.)
                    .finish();
                    item_row.add_child(credential_icon);
                }
                SavePosition::new(item_row.finish(), &position_id).finish()
            }),
            None,
        )
    } else {
        MenuItemFields::new(label).with_icon(leading_icon)
    };

    item = item
        .with_on_select_action(action(llm))
        .with_disabled(llm.disable_reason.is_some());

    if let Some(reason) = &llm.disable_reason {
        item = item
            .with_tooltip(reason.tooltip_text())
            .with_tooltip_position(MenuTooltipPosition::Above);

        if matches!(reason, DisableReason::RequiresUpgrade) {
            item =
                item.with_right_side_label("disabled", Properties::default().style(Style::Italic));
        }
    }

    with_cost_and_profile_info(item, llm, model_id_to_add_profile_default_label_to).into_item()
}

pub fn available_model_menu_items<A: Action + Clone>(
    choices: Vec<&LLMInfo>,
    action: impl Fn(&LLMInfo) -> A,
    model_id_to_add_profile_default_label_to: Option<&LLMId>,
    position_id_fn: Option<&dyn Fn(&LLMId) -> String>,
    collapse_auto: bool,
    collapse_reasoning_variants: bool,
    app: &AppContext,
) -> Vec<MenuItem<A>> {
    choices
        .into_iter()
        .map(|llm| {
            make_item_fields(
                llm,
                &action,
                position_id_fn,
                model_id_to_add_profile_default_label_to,
                collapse_auto,
                collapse_reasoning_variants,
                app,
            )
        })
        .collect_vec()
}
