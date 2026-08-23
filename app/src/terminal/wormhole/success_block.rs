use std::borrow::Cow;
use std::sync::Arc;

use channel_versions::overrides::TargetOS;
use galaxy_core::semantic_selection::SemanticSelection;
use galaxy_core::ui::theme::GalaxyTheme;
use parking_lot::RwLock;
use warpui::elements::{
    Border, Container, CrossAxisAlignment, Flex, Icon, MainAxisAlignment, MainAxisSize,
    ParentElement, SelectableArea, SelectionHandle, Text,
};
use warpui::{AppContext, Element, Entity, SingletonEntity, TypedActionView, View, ViewContext};

use super::render::HORIZONTAL_TEXT_MARGIN;
use super::settings::WormholeSettings;
use super::{render, subshell_bootstrap_success_block_bytes};
use crate::ai::agent::ProgrammingLanguage;
use crate::ai::blocklist::code_block::{render_runnable_code_snippet, CodeSnippetButtonHandles};
use crate::appearance::Appearance;
use crate::terminal::model::terminal_model::SubshellInitializationInfo;
use crate::terminal::shell::{Shell, ShellType};
use crate::ui_components::blended_colors;
use crate::ui_components::icons::Icon as UiIcon;
use crate::workspace::WorkspaceAction;

const VERTICAL_TEXT_MARGIN: f32 = 16.;

#[derive(Debug, Clone)]
pub enum WormholeSuccessBlockEvent {
    OpenWormholeSettings,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub enum WormholeSuccessBlockAction {
    ClearAutoWormholeSnippet,
    OpenWormholeSettings,
}

struct AutoWormholeSnippet {
    /// On subshell initialization, this will contain the output grid to display,
    /// containing info like how to auto-wormhole the subshell.
    output_grid: Cow<'static, str>,
    /// The output grid needs to be selectable to allow users to copy the command to their clipboard.
    selection_handle: SelectionHandle,
    selected_text: Arc<RwLock<Option<String>>>,

    shell_type: ShellType,
    description: Cow<'static, str>,
    code_snippet_handles: CodeSnippetButtonHandles,
    can_write_to_rc: bool,
}

pub struct WormholeSuccessBlock {
    spawning_command: String,
    auto_wormhole_snippet: Option<AutoWormholeSnippet>,
}

impl WormholeSuccessBlock {
    #[allow(clippy::new_without_default)]
    pub fn new(
        spawning_command: String,
        subshell_info: Option<SubshellInitializationInfo>,
        shell: Shell,
        ctx: &mut ViewContext<Self>,
    ) -> Self {
        ctx.subscribe_to_model(&WormholeSettings::handle(ctx), move |_, _, _, ctx| {
            ctx.notify();
        });

        // Mac + Linux have the same behavior. We'd need to handle
        // getting the OS to write to the correct RC file.
        let remote_os = TargetOS::Linux;

        let is_auto_wormhole_configured = subshell_info
            .as_ref()
            .map(|info| info.was_triggered_by_rc_file_snippet)
            .unwrap_or_default();

        let auto_wormhole_snippet = if is_auto_wormhole_configured {
            None
        } else {
            subshell_info.and_then(|subshell_info| {
                // If wormholing wasn't triggered automatically, show a snippet about
                // how to automatically wormhole.
                (!subshell_info.was_triggered_by_rc_file_snippet).then(|| {
                    let (command, is_executable) = subshell_bootstrap_success_block_bytes(
                        &subshell_info,
                        shell.shell_type(),
                        remote_os,
                    );
                    if command.is_empty() {
                        return ("".into(), false);
                    }
                    (
                        String::from_utf8(command)
                            .map(|content| {
                                // Ensure a blank line between the output grid and the learn more link.
                                content + "\n"
                            })
                            .unwrap_or_default(),
                        is_executable,
                    )
                })
            })
        };
        let auto_wormhole_snippet = auto_wormhole_snippet.map(|(output_grid, can_write_to_rc)| {
            AutoWormholeSnippet {
                description: (if !output_grid.is_empty() {
                    "Run the following to automatically Wormhole in the future:"
                } else {
                    "In remote subshells, Galaxy runs commands in the background to power completions, syntax highlighting, and other features."
                }).into(),
                output_grid: output_grid.into(),
                selection_handle: Default::default(),
                selected_text: Default::default(),
                code_snippet_handles: Default::default(),
                shell_type: shell.shell_type(),
                can_write_to_rc,
            }
        });

        Self {
            spawning_command,
            auto_wormhole_snippet,
        }
    }

    pub fn selected_text(&self) -> Option<String> {
        self.auto_wormhole_snippet
            .as_ref()
            .and_then(|snippet| snippet.selected_text.read().clone())
    }

    pub fn render_spawning_command(
        &self,
        theme: &GalaxyTheme,
        appearance: &Appearance,
    ) -> Box<dyn Element> {
        let spawning_command = self.spawning_command.clone();
        render::build_command_row(spawning_command, theme, appearance, true)
            .with_margin_bottom(VERTICAL_TEXT_MARGIN)
            .finish()
    }

    pub fn render_title_ui(
        &self,
        theme: &GalaxyTheme,
        appearance: &Appearance,
    ) -> Box<dyn Element> {
        let header_contents = render::build_header_row(
            "Session Wormholed",
            Icon::new(UiIcon::GalaxyLogo.into(), theme.active_ui_detail()),
            theme,
            appearance,
        )
        .with_margin_right(8.)
        .finish();

        Container::new(
            Flex::row()
                .with_main_axis_alignment(MainAxisAlignment::SpaceBetween)
                .with_cross_axis_alignment(CrossAxisAlignment::End)
                .with_main_axis_size(MainAxisSize::Max)
                .with_child(header_contents)
                .finish(),
        )
        .with_horizontal_margin(HORIZONTAL_TEXT_MARGIN)
        .with_margin_top(VERTICAL_TEXT_MARGIN)
        .finish()
    }

    /// Fired when a block ends and we are not in a Wormholed session.
    pub fn on_wormholed_session_complete(&mut self, ctx: &mut ViewContext<Self>) {
        self.clear_auto_wormhole_snippet(ctx);
    }

    pub fn clear_auto_wormhole_snippet(&mut self, ctx: &mut ViewContext<Self>) {
        self.auto_wormhole_snippet = None;
        ctx.notify();
    }

    /// If there is an output grid to display, render it.
    pub fn render_output_grid(
        &self,
        app: &AppContext,
        appearance: &Appearance,
    ) -> Option<Box<dyn Element>> {
        let theme = appearance.theme();
        let auto_wormhole_snippet = self.auto_wormhole_snippet.as_ref()?;

        if auto_wormhole_snippet.output_grid.is_empty() {
            return None;
        }

        let shell_language = ProgrammingLanguage::Shell(auto_wormhole_snippet.shell_type);
        let runnable_command = render_runnable_code_snippet(
            &auto_wormhole_snippet.output_grid,
            if auto_wormhole_snippet.can_write_to_rc {
                Some(&shell_language)
            } else {
                None
            },
            Some(Box::new({
                move |code_snippet, ctx| {
                    ctx.dispatch_typed_action(WorkspaceAction::RunCommand(
                        code_snippet.to_string(),
                    ));

                    ctx.dispatch_typed_action(WormholeSuccessBlockAction::ClearAutoWormholeSnippet);
                }
            })),
            Some(Box::new({
                move |code_snippet, ctx| {
                    ctx.dispatch_typed_action(WorkspaceAction::CopyTextToClipboard(code_snippet));
                }
            })),
            Some(auto_wormhole_snippet.code_snippet_handles.clone()),
            app,
        );

        let semantic_selection = SemanticSelection::as_ref(app);
        let selected_text = auto_wormhole_snippet.selected_text.clone();

        // TODO(Simon): Implement full selection and copying functionality for the WormholeSuccessBlock.
        // Look to the `EnvVarCollectionBlock` for the existing implementation paradigm. We don't
        // yet have a robust way of ensuring that every aspect of text selection is implemented
        // properly, so be extra careful not to miss any details!
        let output_grid = SelectableArea::new(
            auto_wormhole_snippet.selection_handle.clone(),
            move |selection_args, _, _| {
                *selected_text.write() = selection_args.selection;
            },
            runnable_command,
        )
        .with_word_boundaries_policy(semantic_selection.word_boundary_policy())
        .with_smart_select_fn(semantic_selection.smart_select_fn())
        .finish();

        let output_grid = Flex::column()
            .with_child(
                Container::new(
                    Text::new(
                        auto_wormhole_snippet.description.clone(),
                        appearance.monospace_font_family(),
                        appearance.monospace_font_size(),
                    )
                    .with_color(blended_colors::text_main(theme, theme.background()))
                    .finish(),
                )
                .with_horizontal_margin(HORIZONTAL_TEXT_MARGIN)
                .with_margin_bottom(VERTICAL_TEXT_MARGIN)
                .finish(),
            )
            .with_child(
                Container::new(output_grid)
                    .with_horizontal_margin(HORIZONTAL_TEXT_MARGIN)
                    .with_margin_bottom(VERTICAL_TEXT_MARGIN)
                    .finish(),
            )
            .finish();
        Some(output_grid)
    }
}

impl Entity for WormholeSuccessBlock {
    type Event = WormholeSuccessBlockEvent;
}

pub const WORMHOLE_SUCCESS_BLOCK_VISIBLE_KEY: &str = "WormholeSuccessBlockVisible";

impl View for WormholeSuccessBlock {
    fn ui_name() -> &'static str {
        "WormholeSuccessBlock"
    }

    fn render(&self, app: &AppContext) -> Box<dyn Element> {
        let appearance = Appearance::as_ref(app);
        let theme = appearance.theme();

        let mut content = Flex::column();

        content.add_children([
            self.render_title_ui(theme, appearance),
            self.render_spawning_command(theme, appearance),
        ]);

        if let Some(output_grid) = self.render_output_grid(app, appearance) {
            content.add_child(output_grid);
        }

        Container::new(content.finish())
            .with_background(theme.foreground().with_opacity(10))
            .with_border(Border::top(1.).with_border_fill(theme.outline()))
            .finish()
    }
}

impl TypedActionView for WormholeSuccessBlock {
    type Action = WormholeSuccessBlockAction;

    fn handle_action(&mut self, action: &Self::Action, ctx: &mut ViewContext<Self>) {
        match action {
            WormholeSuccessBlockAction::OpenWormholeSettings => {
                ctx.emit(WormholeSuccessBlockEvent::OpenWormholeSettings);
            }
            WormholeSuccessBlockAction::ClearAutoWormholeSnippet => {
                self.clear_auto_wormhole_snippet(ctx);
            }
        }
    }
}
