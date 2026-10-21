use asset_macro::bundled_asset;
use galaxy_core::ui::theme::GalaxyTheme;
use galaxyui::assets::asset_cache::{AssetCache, AssetState};
use markdown_parser::{FormattedText, FormattedTextFragment, FormattedTextLine};

use crate::ai::blocklist::inline_action::requested_action::RenderableAction;
use crate::appearance::Appearance;
use crate::terminal::shell::ShellType;
use crate::terminal::wormhole;
use crate::ui_components::icons::Icon as UiIcon;
use galaxyui::elements::{HighlightedHyperlink, Hoverable, Icon, MouseStateHandle};
use galaxyui::keymap::FixedBinding;
use galaxyui::AppContext;
use galaxyui::{
    elements::{Border, Container, CrossAxisAlignment, Flex, ParentElement},
    Element, Entity, SingletonEntity, TypedActionView, View, ViewContext,
};

#[derive(Debug, Clone)]
pub enum SshWormholeBlockEvent {
    WormholeSession,
    Cancel,
    Interrupt,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub enum SshWormholeBlockAction {
    Interrupt,
    Focus,
}

pub struct SshWormholeBlock {
    block_mouse_state: MouseStateHandle,
    ssh_command: String,
}

pub fn init(app: &mut AppContext) {
    use galaxyui::keymap::macros::*;

    app.register_fixed_bindings([FixedBinding::new(
        "ctrl-c",
        SshWormholeBlockAction::Interrupt,
        id!(SshWormholeBlock::ui_name()),
    )]);
}

impl SshWormholeBlock {
    #[allow(clippy::new_without_default)]
    pub fn new(ssh_command: String) -> Self {
        Self {
            block_mouse_state: Default::default(),
            ssh_command,
        }
    }

    pub fn focus(&self, ctx: &mut ViewContext<Self>) {
        ctx.focus_self();
        ctx.notify();
    }
}

impl Entity for SshWormholeBlock {
    type Event = SshWormholeBlockEvent;
}

impl SshWormholeBlock {
    fn render_title_ui(&self, theme: &GalaxyTheme, appearance: &Appearance) -> Box<dyn Element> {
        let icon = Icon::new(UiIcon::GalaxyLogo.into(), theme.active_ui_detail());
        wormhole::render::header_row("Wormholing SSH Session...", icon, theme, appearance)
    }
}

pub fn wormhole_description(
    app: &AppContext,
    hyperlink_index: &HighlightedHyperlink,
) -> Box<dyn Element> {
    let appearance = Appearance::as_ref(app);
    let theme = appearance.theme();

    let description = FormattedText::new(vec![FormattedTextLine::Line(vec![
        FormattedTextFragment::plain_text(
            "Bring Galaxy's features to your remote session: blocks, full text editing, completions, Galaxy Agent, and more."
        ),
    ])]);
    wormhole::render::build_description_row(description, theme, appearance, hyperlink_index.clone())
        .finish()
}

impl View for SshWormholeBlock {
    fn ui_name() -> &'static str {
        "SshWormholeBlock"
    }

    fn render(&self, app: &AppContext) -> Box<dyn Element> {
        let appearance = Appearance::as_ref(app);
        let theme = appearance.theme();

        let mut content = Flex::column().with_cross_axis_alignment(CrossAxisAlignment::Stretch);

        content.add_child(self.render_title_ui(theme, appearance));

        content.add_child(
            Container::new(
                RenderableAction::new(&self.ssh_command, app)
                    .with_background_color(theme.background().into_solid())
                    .render(app)
                    .finish(),
            )
            .with_margin_top(16.)
            .finish(),
        );

        Hoverable::new(self.block_mouse_state.clone(), |_| {
            Container::new(content.finish())
                .with_padding_top(10.)
                .with_background(theme.foreground().with_opacity(10))
                .with_border(Border::top(1.).with_border_fill(theme.outline()))
                .finish()
        })
        .on_click(|ctx, _, _| {
            ctx.dispatch_typed_action(SshWormholeBlockAction::Focus);
        })
        .finish()
    }
}

impl TypedActionView for SshWormholeBlock {
    type Action = SshWormholeBlockAction;

    fn handle_action(&mut self, action: &Self::Action, ctx: &mut ViewContext<Self>) {
        match action {
            SshWormholeBlockAction::Interrupt => {
                ctx.emit(SshWormholeBlockEvent::Interrupt);
            }
            SshWormholeBlockAction::Focus => {
                self.focus(ctx);
            }
        }
    }
}

/// Convert the begin_wormhole_ssh_session script into a string.
pub fn begin_wormhole_ssh_session_command(app: &AppContext) -> String {
    let asset = bundled_asset!("bootstrap/unknown_init_subshell.sh");

    match AssetCache::as_ref(app).load_asset::<String>(asset) {
        AssetState::Loaded { data } => data.to_string().replace("HOOK_NAME", "InitSsh"),
        _ => panic!("ssh begin wormhole script should be available as a string"),
    }
}

/// Convert the wormhole_ssh_session script into a string.
pub fn wormhole_ssh_session_command(
    uname: &str,
    shell_type: ShellType,
    app: &AppContext,
) -> Option<String> {
    let asset = match (uname, shell_type) {
        // Mac scripts must be less than 1020 characters due to macOS 15+ pty issue
        ("Darwin", ShellType::Zsh | ShellType::Bash) => {
            bundled_asset!("ssh/bash_zsh/wormhole_ssh_session_mac.sh")
        }
        // Mac scripts must be less than 1020 characters due to macOS 15+ pty issue
        ("Darwin", ShellType::Fish) => bundled_asset!("ssh/fish/wormhole_ssh_session_mac.sh"),
        (_, ShellType::Zsh | ShellType::Bash) => {
            bundled_asset!("ssh/bash_zsh/wormhole_ssh_session.sh")
        }
        (_, ShellType::Fish) => bundled_asset!("ssh/fish/wormhole_ssh_session.sh"),
        // PowerShell is not supported yet.
        (_, ShellType::PowerShell) => return None,
    };

    // Todo(Jack): look into avoiding an allocation here.
    match AssetCache::as_ref(app).load_asset::<String>(asset) {
        AssetState::Loaded { data } => Some(data.to_string()),
        _ => panic!("ssh wormhole script should be available as a string"),
    }
}
#[cfg(test)]
#[path = "wormhole_test.rs"]
mod tests;
