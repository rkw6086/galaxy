//! The project-scoped history and log browser for agent-started background commands.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use std::time::{Duration, Instant};

use galaxy_core::ui::icons::Icon;
use galaxy_core::ui::theme::{color::internal_colors, TerminalColors};
use galaxy_core::HostId;
use galaxy_util::local_or_remote_path::LocalOrRemotePath;
use galaxy_util::standardized_path::StandardizedPath;
use galaxyui::elements::{
    Border, ChildView, Clipped, ClippedScrollStateHandle, ClippedScrollable, ConstrainedBox,
    Container, CrossAxisAlignment, Expanded, Flex, Hoverable, MainAxisAlignment, MainAxisSize,
    MouseStateHandle, Padding, ParentElement, Point, RowBackground, ScrollbarWidth, Shrinkable,
    Table, TableColumnWidth, TableConfig, TableHeader, TableStateHandle, TableVerticalSizing, Text,
};
use galaxyui::event::DispatchedEvent;
use galaxyui::fonts::{Properties, Weight};
use galaxyui::platform::Cursor;
use galaxyui::r#async::Timer;
use galaxyui::text_layout::ClipConfig;
use galaxyui::{
    AfterLayoutContext, AppContext, Element, Entity, EventContext, LayoutContext, PaintContext,
    SingletonEntity, SizeConstraint, TypedActionView, View, ViewContext, ViewHandle,
};
use pathfinder_geometry::vector::Vector2F;
use uuid::Uuid;

use super::{BackgroundJob, BackgroundJobStatus, BackgroundJobsModel};
use crate::appearance::Appearance;
use crate::editor::{
    EditorOptions, EditorView, Event as EditorEvent, InteractionState, PlainTextEditorViewAction,
    SingleLineEditorOptions, TextColors, TextOptions,
};
use crate::modal::ModalAction;
use crate::view_components::action_button::{
    ActionButton, ButtonSize, DangerSecondaryTheme, NakedTheme, SecondaryTheme,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JobScopeFilter {
    Project,
    Directory,
    All,
}

#[derive(Clone, Debug)]
pub enum BackgroundJobsAction {
    SetScope(JobScopeFilter),
    SetStatus(Option<BackgroundJobStatus>),
    Select(Uuid),
    CancelSelected,
    Cancel(Uuid),
    ViewLogs(Uuid),
    Back,
    ReloadLogs,
    LoadMore,
}

#[derive(Clone)]
struct JobRowControls {
    mouse: MouseStateHandle,
    view_logs: ViewHandle<ActionButton>,
    terminate: ViewHandle<ActionButton>,
    logs_available: bool,
}

pub struct BackgroundJobsView {
    working_directory: Option<PathBuf>,
    project_root: Option<PathBuf>,
    host_id: Option<HostId>,
    scope: JobScopeFilter,
    status: Option<BackgroundJobStatus>,
    selected: Option<Uuid>,
    search: ViewHandle<EditorView>,
    logs: ViewHandle<EditorView>,
    scope_buttons: Vec<(JobScopeFilter, ViewHandle<ActionButton>)>,
    status_buttons: Vec<(Option<BackgroundJobStatus>, ViewHandle<ActionButton>)>,
    cancel_button: ViewHandle<ActionButton>,
    reload_button: ViewHandle<ActionButton>,
    rows: HashMap<Uuid, JobRowControls>,
    logs_open: bool,
    visible: bool,
    refresh_scheduled: bool,
    display_limit: usize,
    back_button: ViewHandle<ActionButton>,
    more_button: ViewHandle<ActionButton>,
    list_scroll: ClippedScrollStateHandle,
    table_state: TableStateHandle,
    logs_scroll: ClippedScrollStateHandle,
    command_scroll: ClippedScrollStateHandle,
    displayed_logs: String,
    displayed_log_colors: Option<TerminalColors>,
    error: Option<String>,
}

impl BackgroundJobsView {
    pub fn new(ctx: &mut ViewContext<Self>) -> Self {
        let search = ctx.add_typed_action_view(|ctx| {
            let mut editor = EditorView::single_line(SingleLineEditorOptions::default(), ctx);
            editor.set_placeholder_text("Search commands or project paths", ctx);
            editor
        });
        ctx.subscribe_to_view(&search, |me, _, event, ctx| match event {
            EditorEvent::Edited(_) => me.synchronize(ctx),
            EditorEvent::Escape => ctx.dispatch_typed_action(&ModalAction::Close),
            _ => {}
        });
        let logs_font = Appearance::as_ref(ctx).monospace_font_family();
        let logs_size = Appearance::as_ref(ctx).monospace_font_size();
        let logs = ctx.add_typed_action_view(|ctx| {
            let mut editor = EditorView::new(
                EditorOptions {
                    text: TextOptions {
                        font_family_override: Some(logs_font),
                        font_size_override: Some(logs_size),
                        ..Default::default()
                    },
                    autogrow: true,
                    soft_wrap: true,
                    supports_vim_mode: false,
                    use_settings_line_height_ratio: true,
                    ..Default::default()
                },
                ctx,
            );
            editor.set_interaction_state(InteractionState::Selectable, ctx);
            editor
        });
        ctx.subscribe_to_view(&logs, |_, _, event, ctx| {
            if matches!(event, EditorEvent::Escape) {
                ctx.dispatch_typed_action(&ModalAction::Close);
            }
        });
        let scope_buttons = [
            (JobScopeFilter::Project, "Current project"),
            (JobScopeFilter::Directory, "Current directory"),
            (JobScopeFilter::All, "All projects"),
        ]
        .into_iter()
        .map(|(scope, label)| {
            let button = ctx.add_typed_action_view(|_| {
                ActionButton::new(label, NakedTheme)
                    .with_size(ButtonSize::Small)
                    .on_click(move |ctx| {
                        ctx.dispatch_typed_action(BackgroundJobsAction::SetScope(scope))
                    })
            });
            (scope, button)
        })
        .collect();
        let status_buttons = [
            (None, "All statuses"),
            (Some(BackgroundJobStatus::Running), "Running"),
            (Some(BackgroundJobStatus::Succeeded), "Succeeded"),
            (Some(BackgroundJobStatus::Failed), "Failed"),
            (Some(BackgroundJobStatus::Cancelled), "Canceled"),
        ]
        .into_iter()
        .map(|(status, label)| {
            let value = status.clone();
            let button = ctx.add_typed_action_view(|_| {
                ActionButton::new(label, NakedTheme)
                    .with_size(ButtonSize::Small)
                    .on_click(move |ctx| {
                        ctx.dispatch_typed_action(BackgroundJobsAction::SetStatus(value.clone()))
                    })
            });
            (status, button)
        })
        .collect();
        let cancel_button = ctx.add_typed_action_view(|_| {
            ActionButton::new("Terminate", DangerSecondaryTheme)
                .with_size(ButtonSize::Small)
                .with_icon(Icon::StopFilled)
                .on_click(|ctx| ctx.dispatch_typed_action(BackgroundJobsAction::CancelSelected))
        });
        let reload_button = ctx.add_typed_action_view(|_| {
            ActionButton::new("Reload logs", SecondaryTheme)
                .with_size(ButtonSize::Small)
                .on_click(|ctx| ctx.dispatch_typed_action(BackgroundJobsAction::ReloadLogs))
        });
        let back_button = ctx.add_typed_action_view(|_| {
            ActionButton::new("Back to jobs", NakedTheme)
                .with_icon(Icon::ArrowLeft)
                .with_size(ButtonSize::Small)
                .on_click(|ctx| ctx.dispatch_typed_action(BackgroundJobsAction::Back))
        });
        let more_button = ctx.add_typed_action_view(|_| {
            ActionButton::new("Show more jobs", SecondaryTheme)
                .with_size(ButtonSize::Small)
                .on_click(|ctx| ctx.dispatch_typed_action(BackgroundJobsAction::LoadMore))
        });
        ctx.subscribe_to_model(&BackgroundJobsModel::handle(ctx), |me, _, _, ctx| {
            if me.visible {
                me.synchronize(ctx);
            }
        });
        ctx.subscribe_to_model(&Appearance::handle(ctx), |me, _, _, ctx| {
            if me.visible && me.logs_open {
                me.set_logs(me.displayed_logs.clone(), ctx);
                ctx.notify();
            }
        });

        Self {
            working_directory: None,
            project_root: None,
            host_id: None,
            scope: JobScopeFilter::Directory,
            status: None,
            selected: None,
            search,
            logs,
            scope_buttons,
            status_buttons,
            cancel_button,
            reload_button,
            rows: HashMap::new(),
            logs_open: false,
            visible: false,
            refresh_scheduled: false,
            display_limit: 100,
            back_button,
            more_button,
            list_scroll: Default::default(),
            table_state: TableStateHandle::new(0, |_, _| Vec::new()),
            logs_scroll: Default::default(),
            command_scroll: Default::default(),
            displayed_logs: String::new(),
            displayed_log_colors: None,
            error: None,
        }
    }

    pub fn on_open(
        &mut self,
        working_directory: Option<LocalOrRemotePath>,
        project_root: Option<LocalOrRemotePath>,
        host_id: Option<HostId>,
        selected: Option<Uuid>,
        ctx: &mut ViewContext<Self>,
    ) {
        let native_path = |path: LocalOrRemotePath| match path {
            LocalOrRemotePath::Local(path) => std::fs::canonicalize(&path).unwrap_or(path),
            LocalOrRemotePath::Remote(path) => PathBuf::from(path.path.as_str()),
        };
        let local_directory = matches!(working_directory, Some(LocalOrRemotePath::Local(_)));
        self.working_directory = working_directory.map(native_path);
        self.project_root = project_root.map(native_path);
        if self.project_root.is_none() && local_directory {
            self.project_root = self.working_directory.as_ref().and_then(|directory| {
                directory
                    .ancestors()
                    .find(|parent| parent.join(".git").exists())
                    .map(Path::to_path_buf)
            });
        }
        self.host_id = host_id;
        self.scope = if self.project_root.is_some() {
            JobScopeFilter::Project
        } else {
            JobScopeFilter::Directory
        };
        self.status = None;
        self.selected = selected;
        self.logs_open =
            selected.is_some_and(|id| BackgroundJobsModel::as_ref(ctx).job(id).is_some());
        self.visible = true;
        self.display_limit = 100;
        self.error = None;
        self.logs_scroll = Default::default();
        self.command_scroll = Default::default();
        self.search.update(ctx, |editor, ctx| {
            editor.clear_buffer_and_reset_undo_stack(ctx)
        });
        // A link from a tool card must reveal its job even after the shell changes directory.
        if let Some(job) = selected
            .and_then(|id| BackgroundJobsModel::as_ref(ctx).job(id))
            .cloned()
        {
            self.working_directory = Some(job.working_directory.clone());
            self.project_root = job.project_root.clone();
            self.host_id = job.host_id.clone();
            self.scope = if self.project_root.is_some() {
                JobScopeFilter::Project
            } else {
                JobScopeFilter::Directory
            };
        }
        self.synchronize(ctx);
        self.schedule_log_availability_refresh(ctx);
        if self.logs_open {
            ctx.focus(&self.logs);
        } else {
            ctx.focus(&self.search);
        }
    }

    pub fn on_close(&mut self) {
        self.visible = false;
        self.logs_open = false;
    }

    fn schedule_log_availability_refresh(&mut self, ctx: &mut ViewContext<Self>) {
        if self.refresh_scheduled || !self.visible {
            return;
        }
        self.refresh_scheduled = true;
        ctx.spawn(
            async { Timer::after(Duration::from_secs(1)).await },
            |me, _, ctx| {
                me.refresh_scheduled = false;
                if !me.visible {
                    return;
                }
                // Temporary files may be removed without a job-model event. Refresh only
                // availability while this browser is open; command monitoring stays independent.
                let changed = me.rows.iter().any(|(id, controls)| {
                    controls.logs_available != BackgroundJobsModel::as_ref(ctx).logs_available(*id)
                });
                if changed {
                    me.synchronize(ctx);
                }
                me.schedule_log_availability_refresh(ctx);
            },
        );
    }

    fn filtered_jobs(&self, app: &AppContext) -> Vec<Uuid> {
        let query = self.search.as_ref(app).buffer_text(app).to_lowercase();
        let mut jobs = BackgroundJobsModel::as_ref(app)
            .records()
            .filter(|job| {
                let scope_matches = job_matches_scope(
                    job,
                    self.scope,
                    self.working_directory.as_deref(),
                    self.project_root.as_deref(),
                    self.host_id.as_ref(),
                );
                scope_matches
                    && self
                        .status
                        .as_ref()
                        .is_none_or(|status| &job.status == status)
                    && (query.is_empty()
                        || job.command.to_lowercase().contains(&query)
                        || job
                            .working_directory
                            .to_string_lossy()
                            .to_lowercase()
                            .contains(&query)
                        || job.project_root.as_ref().is_some_and(|root| {
                            root.to_string_lossy().to_lowercase().contains(&query)
                        }))
            })
            .collect::<Vec<_>>();
        jobs.sort_by(|a, b| {
            b.started_at
                .cmp(&a.started_at)
                .then_with(|| a.id.cmp(&b.id))
        });
        jobs.into_iter().map(|job| job.id).collect()
    }

    fn synchronize(&mut self, ctx: &mut ViewContext<Self>) {
        let jobs = self.filtered_jobs(ctx);
        if let Some(index) = self
            .selected
            .and_then(|id| jobs.iter().position(|job_id| *job_id == id))
        {
            self.display_limit = self.display_limit.max(index + 1);
        }
        let ids = jobs
            .iter()
            .take(self.display_limit)
            .copied()
            .collect::<HashSet<_>>();
        self.rows.retain(|id, _| ids.contains(id));
        for id in ids {
            if !self.rows.contains_key(&id) {
                let view_logs = ctx.add_typed_action_view(|_| {
                    ActionButton::new("View logs", SecondaryTheme)
                        .with_size(ButtonSize::Small)
                        .on_click(move |ctx| {
                            ctx.dispatch_typed_action(BackgroundJobsAction::ViewLogs(id))
                        })
                });
                let terminate = ctx.add_typed_action_view(|_| {
                    ActionButton::new("Terminate", DangerSecondaryTheme)
                        .with_size(ButtonSize::Small)
                        .with_icon(Icon::StopFilled)
                        .on_click(move |ctx| {
                            ctx.dispatch_typed_action(BackgroundJobsAction::Cancel(id))
                        })
                });
                self.rows.insert(
                    id,
                    JobRowControls {
                        mouse: Default::default(),
                        view_logs,
                        terminate,
                        logs_available: false,
                    },
                );
            }
            if let Some(row) = self.rows.get_mut(&id) {
                row.logs_available = BackgroundJobsModel::as_ref(ctx).logs_available(id);
            }
        }
        if self.selected.is_none_or(|id| !jobs.contains(&id)) {
            self.selected = jobs.first().copied();
        }
        for (value, button) in &self.scope_buttons {
            button.update(ctx, |button, ctx| {
                button.set_active(*value == self.scope, ctx);
                button.set_disabled(
                    *value == JobScopeFilter::Project && self.project_root.is_none()
                        || *value == JobScopeFilter::Directory && self.working_directory.is_none(),
                    ctx,
                );
            });
        }
        for (value, button) in &self.status_buttons {
            button.update(ctx, |button, ctx| {
                button.set_active(*value == self.status, ctx)
            });
        }
        let selected = self
            .selected
            .is_some_and(|id| BackgroundJobsModel::as_ref(ctx).job(id).is_some());
        let running = self
            .selected
            .and_then(|id| BackgroundJobsModel::as_ref(ctx).job(id))
            .is_some_and(|job| job.status == BackgroundJobStatus::Running);
        self.cancel_button
            .update(ctx, |button, ctx| button.set_disabled(!running, ctx));
        self.reload_button
            .update(ctx, |button, ctx| button.set_disabled(!selected, ctx));
        if self.logs_open {
            self.load_selected_logs(ctx);
        }
        ctx.notify();
    }

    fn set_logs(&mut self, text: String, ctx: &mut ViewContext<Self>) {
        let theme = Appearance::as_ref(ctx).theme();
        let colors = theme.terminal_colors().clone();
        let foreground = theme.foreground();
        self.logs.update(ctx, |editor, ctx| {
            editor.set_text_colors(
                TextColors {
                    default_color: foreground,
                    disabled_color: foreground,
                    hint_color: foreground,
                },
                ctx,
            );
        });
        if text != self.displayed_logs || self.displayed_log_colors.as_ref() != Some(&colors) {
            let (plain, styles) = super::log_format::format_job_logs(&text, &colors);
            self.logs.update(ctx, |editor, ctx| {
                // Programmatic replacement must work while the viewer remains read-only.
                let interaction = editor.interaction_state(ctx);
                editor.set_interaction_state(InteractionState::Editable, ctx);
                editor.clear_buffer_and_reset_undo_stack(ctx);
                editor.insert_with_styles(
                    &plain,
                    &styles,
                    PlainTextEditorViewAction::ReplaceBuffer,
                    ctx,
                );
                editor.set_interaction_state(interaction, ctx);
            });
            self.displayed_logs = text;
            self.displayed_log_colors = Some(colors);
        }
    }

    fn load_selected_logs(&mut self, ctx: &mut ViewContext<Self>) {
        let logs = match self.selected {
            Some(id) if BackgroundJobsModel::as_ref(ctx).logs_available(id) => BackgroundJobsModel::as_ref(ctx).read_logs(id).unwrap_or_else(|_| "The temporary log file is no longer available. Job history and status are still saved.".to_owned()),
            Some(_) => "The temporary log file is no longer available. Job history and status are still saved.".to_owned(),
            None => String::new(),
        };
        self.set_logs(logs, ctx);
    }

    fn render_list(&self, app: &AppContext) -> Box<dyn Element> {
        let appearance = Appearance::as_ref(app);
        let theme = appearance.theme();
        let jobs = self
            .filtered_jobs(app)
            .into_iter()
            .take(self.display_limit)
            .collect::<Vec<_>>();
        if jobs.is_empty() {
            return Container::new(
                Text::new(
                    "No background jobs match these filters.",
                    appearance.ui_font_family(),
                    appearance.ui_font_size(),
                )
                .with_color(theme.sub_text_color(theme.background()).into())
                .finish(),
            )
            .with_padding(Padding::uniform(24.))
            .finish();
        }
        let headers = [
            ("Command", TableColumnWidth::Flex(1.5)),
            ("Directory", TableColumnWidth::Flex(1.)),
            ("Started", TableColumnWidth::Fixed(126.)),
            ("Status", TableColumnWidth::Fixed(156.)),
            ("Actions", TableColumnWidth::Fixed(216.)),
        ]
        .into_iter()
        .map(|(label, width)| {
            TableHeader::new(
                Text::new(
                    label,
                    appearance.ui_font_family(),
                    appearance.ui_font_size() - 1.,
                )
                .with_style(Properties::default().weight(Weight::Semibold))
                .with_color(theme.nonactive_ui_text_color().into())
                .finish(),
            )
            .with_width(width)
        })
        .collect();
        let rows = self.rows.clone();
        let table = Table::new(self.table_state.clone(), 0., 0.)
            .with_headers(headers)
            .with_row_count(jobs.len())
            .with_row_render_fn(move |index, app| {
                let appearance = Appearance::as_ref(app);
                let theme = appearance.theme();
                let id = jobs[index];
                let Some(job) = BackgroundJobsModel::as_ref(app).job(id) else {
                    return Vec::new();
                };
                let controls = &rows[&id];
                let command = job
                    .command
                    .lines()
                    .next()
                    .unwrap_or(&job.command)
                    .to_owned();
                let command_cell = Hoverable::new(controls.mouse.clone(), move |_| {
                    Text::new_inline(
                        command,
                        appearance.monospace_font_family(),
                        appearance.ui_font_size(),
                    )
                    .with_color(theme.active_ui_text_color().into())
                    .with_clip(ClipConfig::ellipsis())
                    .finish()
                })
                .on_click(move |ctx, _, _| {
                    ctx.dispatch_typed_action(BackgroundJobsAction::ViewLogs(id))
                })
                .with_cursor(Cursor::PointingHand)
                .finish();
                let directory = match &job.host_id {
                    Some(host) => format!("{host}: {}", job.working_directory.display()),
                    None => job.working_directory.display().to_string(),
                };
                let directory = Text::new_inline(
                    directory,
                    appearance.ui_font_family(),
                    appearance.ui_font_size() - 1.,
                )
                .with_color(theme.sub_text_color(theme.surface_1()).into())
                .with_clip(ClipConfig::ellipsis())
                .finish();
                let started = Text::new(
                    job.started_at
                        .with_timezone(&chrono::Local)
                        .format("%b %d, %H:%M")
                        .to_string(),
                    appearance.ui_font_family(),
                    appearance.ui_font_size() - 1.,
                )
                .with_color(theme.sub_text_color(theme.surface_1()).into())
                .finish();
                let status = Flex::row()
                    .with_spacing(6.)
                    .with_cross_axis_alignment(CrossAxisAlignment::Center)
                    .with_child(status_icon(&job.status, app))
                    .with_child(
                        Shrinkable::new(
                            1.,
                            Text::new(
                                status_label(job),
                                appearance.ui_font_family(),
                                appearance.ui_font_size() - 1.,
                            )
                            .with_color(theme.active_ui_text_color().into())
                            .finish(),
                        )
                        .finish(),
                    )
                    .finish();
                let mut actions = Flex::row()
                    .with_spacing(4.)
                    .with_cross_axis_alignment(CrossAxisAlignment::Center);
                if controls.logs_available && BackgroundJobsModel::as_ref(app).logs_available(id) {
                    actions.add_child(ChildView::new(&controls.view_logs).finish());
                }
                if job.status == BackgroundJobStatus::Running {
                    actions.add_child(ChildView::new(&controls.terminate).finish());
                }
                vec![command_cell, directory, started, status, actions.finish()]
            })
            .with_config(TableConfig {
                border_width: 1.,
                border_color: theme.outline().into(),
                outer_border: true,
                column_dividers: false,
                row_dividers: true,
                cell_padding: 12.,
                header_background: theme.surface_2().into(),
                row_background: RowBackground::uniform(theme.surface_1().into()),
                fixed_header: false,
                vertical_sizing: TableVerticalSizing::ExpandToContent,
                measure_body_cells_for_intrinsic_widths: false,
            });
        ClippedScrollable::vertical(
            self.list_scroll.clone(),
            table.finish(),
            ScrollbarWidth::Auto,
            theme.nonactive_ui_detail().into(),
            theme.active_ui_detail().into(),
            galaxyui::elements::Fill::None,
        )
        .finish()
    }

    fn render_detail(&self, app: &AppContext) -> Box<dyn Element> {
        let appearance = Appearance::as_ref(app);
        let theme = appearance.theme();
        let mut detail = Flex::column()
            .with_cross_axis_alignment(CrossAxisAlignment::Stretch)
            .with_main_axis_size(MainAxisSize::Max)
            .with_spacing(12.);
        let selected = self
            .selected
            .and_then(|id| BackgroundJobsModel::as_ref(app).job(id));
        let mut header = Flex::row()
            .with_cross_axis_alignment(CrossAxisAlignment::Center)
            .with_spacing(8.)
            .with_child(ChildView::new(&self.back_button).finish());
        if let Some(job) = selected {
            header.add_child(
                Expanded::new(
                    1.,
                    Flex::row()
                        .with_spacing(6.)
                        .with_cross_axis_alignment(CrossAxisAlignment::Center)
                        .with_child(status_icon(&job.status, app))
                        .with_child(
                            Text::new(
                                status_label(job),
                                appearance.ui_font_family(),
                                appearance.ui_font_size(),
                            )
                            .with_style(Properties::default().weight(Weight::Semibold))
                            .with_color(theme.active_ui_text_color().into())
                            .finish(),
                        )
                        .finish(),
                )
                .finish(),
            );
            if BackgroundJobsModel::as_ref(app).logs_available(job.id) {
                header.add_child(ChildView::new(&self.reload_button).finish());
            }
            if job.status == BackgroundJobStatus::Running {
                header.add_child(ChildView::new(&self.cancel_button).finish());
            }
        }
        detail.add_child(header.finish());
        let Some(job) = selected else {
            detail.add_child(
                Text::new(
                    "Select a job to view its command, status, and logs.",
                    appearance.ui_font_family(),
                    appearance.ui_font_size(),
                )
                .finish(),
            );
            return detail.finish();
        };
        let label = |text: String| {
            Text::new(
                text,
                appearance.ui_font_family(),
                appearance.ui_font_size() - 1.,
            )
            .with_color(theme.sub_text_color(theme.surface_2()).into())
            .finish()
        };
        detail.add_child(label(format!(
            "{}  ·  Started {}",
            job.working_directory.display(),
            job.started_at
                .with_timezone(&chrono::Local)
                .format("%b %d, %H:%M:%S")
        )));
        let command = Text::new(
            job.command.clone(),
            appearance.monospace_font_family(),
            appearance.ui_font_size(),
        )
        .soft_wrap(true)
        .with_color(theme.active_ui_text_color().into())
        .finish();
        let command = ClippedScrollable::vertical(
            self.command_scroll.clone(),
            command,
            ScrollbarWidth::Auto,
            theme.nonactive_ui_detail().into(),
            theme.active_ui_detail().into(),
            galaxyui::elements::Fill::None,
        )
        .finish();
        detail.add_child(
            Container::new(ConstrainedBox::new(command).with_max_height(120.).finish())
                .with_padding(Padding::uniform(12.))
                .with_background(theme.surface_2())
                .with_border(Border::all(1.).with_border_color(theme.outline().into()))
                .finish(),
        );
        if let Some(error) = self.error.as_ref().or(job.error.as_ref()) {
            detail.add_child(
                Text::new(
                    error.clone(),
                    appearance.ui_font_family(),
                    appearance.ui_font_size(),
                )
                .with_color(theme.ui_error_color().into())
                .finish(),
            );
        }
        detail.add_child(
            Flex::row()
                .with_main_axis_size(MainAxisSize::Max)
                .with_main_axis_alignment(MainAxisAlignment::SpaceBetween)
                .with_child(
                    Text::new(
                        "Output",
                        appearance.ui_font_family(),
                        appearance.ui_font_size(),
                    )
                    .with_style(Properties::default().weight(Weight::Semibold))
                    .with_color(theme.active_ui_text_color().into())
                    .finish(),
                )
                .with_child(label("Select text to copy".to_owned()))
                .finish(),
        );
        let logs = ClippedScrollable::vertical(
            self.logs_scroll.clone(),
            Container::new(ChildView::new(&self.logs).finish())
                .with_padding(Padding::uniform(12.))
                .finish(),
            ScrollbarWidth::Auto,
            theme.nonactive_ui_detail().into(),
            theme.active_ui_detail().into(),
            galaxyui::elements::Fill::None,
        )
        .finish();
        detail.add_child(
            Expanded::new(
                1.,
                Container::new(logs)
                    .with_background(theme.background())
                    .with_border(Border::all(1.).with_border_color(theme.outline().into()))
                    .finish(),
            )
            .finish(),
        );
        detail.finish()
    }
}

pub fn status_icon(status: &BackgroundJobStatus, app: &AppContext) -> Box<dyn Element> {
    let theme = Appearance::as_ref(app).theme();
    let icon = match status {
        BackgroundJobStatus::Running => {
            return ConstrainedBox::new(
                IndeterminateJobIndicator::new(internal_colors::neutral_6(theme).into()).finish(),
            )
            .with_width(16.)
            .with_height(16.)
            .finish()
        }
        BackgroundJobStatus::Succeeded => {
            Icon::Check.to_galaxyui_icon(theme.ui_green_color().into())
        }
        BackgroundJobStatus::Failed => {
            Icon::XCircle.to_galaxyui_icon(theme.ui_error_color().into())
        }
        BackgroundJobStatus::Cancelled => {
            Icon::StopFilled.to_galaxyui_icon(internal_colors::neutral_5(theme).into())
        }
    };
    ConstrainedBox::new(icon.finish())
        .with_width(16.)
        .with_height(16.)
        .finish()
}

fn job_matches_scope(
    job: &BackgroundJob,
    scope: JobScopeFilter,
    working_directory: Option<&Path>,
    project_root: Option<&Path>,
    host_id: Option<&HostId>,
) -> bool {
    match scope {
        JobScopeFilter::Project => project_root.is_some_and(|root| {
            job.host_id.as_ref() == host_id
                && job
                    .project_root
                    .as_deref()
                    .is_some_and(|job_root| scope_paths_match(job_root, root, host_id.is_some()))
        }),
        JobScopeFilter::Directory => working_directory.is_some_and(|directory| {
            job.host_id.as_ref() == host_id
                && scope_paths_match(&job.working_directory, directory, host_id.is_some())
        }),
        JobScopeFilter::All => true,
    }
}

fn scope_paths_match(left: &Path, right: &Path, remote: bool) -> bool {
    if !remote {
        return left == right;
    }
    let normalize = |path: &Path| {
        let value = path.to_str()?;
        // Windows canonicalization adds a verbatim prefix. Preserve the remote drive/UNC
        // identity while comparing it to shell metadata on clients using another OS.
        let value = if let Some(unc) = value
            .strip_prefix(r"\\?\UNC\")
            .or_else(|| value.strip_prefix("//?/UNC/"))
        {
            format!(r"\\{unc}")
        } else {
            value
                .strip_prefix(r"\\?\")
                .or_else(|| value.strip_prefix("//?/"))
                .unwrap_or(value)
                .to_owned()
        };
        StandardizedPath::try_new(&value).ok()
    };
    match (normalize(left), normalize(right)) {
        (Some(left), Some(right)) => left == right,
        _ => left == right,
    }
}

/// Existing loading-dot artwork, advanced at paint time so waiting jobs keep moving even when
/// they produce no output. This schedules UI repaint only; it never polls a process or model.
struct IndeterminateJobIndicator {
    frames: [galaxyui::elements::Icon; 4],
    current_frame: usize,
}

impl IndeterminateJobIndicator {
    fn new(color: galaxy_core::ui::theme::Fill) -> Self {
        Self {
            frames: [
                Icon::LoadingDots0,
                Icon::LoadingDots1,
                Icon::LoadingDots2,
                Icon::LoadingDots3,
            ]
            .map(|icon| icon.to_galaxyui_icon(color)),
            current_frame: 0,
        }
    }
}

impl Element for IndeterminateJobIndicator {
    fn layout(
        &mut self,
        constraint: SizeConstraint,
        ctx: &mut LayoutContext,
        app: &AppContext,
    ) -> Vector2F {
        let mut size = Vector2F::zero();
        for frame in &mut self.frames {
            size = frame.layout(constraint, ctx, app);
        }
        size
    }

    fn after_layout(&mut self, ctx: &mut AfterLayoutContext, app: &AppContext) {
        for frame in &mut self.frames {
            frame.after_layout(ctx, app);
        }
    }

    fn paint(&mut self, origin: Vector2F, ctx: &mut PaintContext, app: &AppContext) {
        static STARTED: LazyLock<Instant> = LazyLock::new(Instant::now);
        self.current_frame = (STARTED.elapsed().as_millis() / 180 % 4) as usize;
        self.frames[self.current_frame].paint(origin, ctx, app);
        ctx.repaint_after(Duration::from_millis(180));
    }

    fn size(&self) -> Option<Vector2F> {
        self.frames[self.current_frame].size()
    }
    fn origin(&self) -> Option<Point> {
        self.frames[self.current_frame].origin()
    }
    fn dispatch_event(
        &mut self,
        event: &DispatchedEvent,
        ctx: &mut EventContext,
        app: &AppContext,
    ) -> bool {
        self.frames[self.current_frame].dispatch_event(event, ctx, app)
    }
}

fn status_label(job: &BackgroundJob) -> String {
    match job.status {
        BackgroundJobStatus::Running => "Running".to_owned(),
        BackgroundJobStatus::Succeeded => "Succeeded · exit 0".to_owned(),
        BackgroundJobStatus::Failed => format!(
            "Failed · exit {}",
            job.exit_code
                .map(|code| code.to_string())
                .unwrap_or_else(|| "unavailable".to_owned())
        ),
        BackgroundJobStatus::Cancelled => "Canceled".to_owned(),
    }
}

impl Entity for BackgroundJobsView {
    type Event = ();
}

impl View for BackgroundJobsView {
    fn ui_name() -> &'static str {
        "BackgroundJobsView"
    }

    fn render(&self, app: &AppContext) -> Box<dyn Element> {
        if self.logs_open {
            return self.render_detail(app);
        }
        let appearance = Appearance::as_ref(app);
        let theme = appearance.theme();
        let mut scopes = Flex::row()
            .with_spacing(4.)
            .with_cross_axis_alignment(CrossAxisAlignment::Center);
        for (_, button) in &self.scope_buttons {
            scopes.add_child(ChildView::new(button).finish());
        }
        let search = Container::new(Clipped::new(ChildView::new(&self.search).finish()).finish())
            .with_horizontal_padding(10.)
            .with_vertical_padding(8.)
            .with_border(Border::all(1.).with_border_color(theme.outline().into()))
            .finish();
        let mut statuses = Flex::row().with_spacing(4.);
        for (_, button) in &self.status_buttons {
            statuses.add_child(ChildView::new(button).finish());
        }
        let scope_text = match self.scope {
            JobScopeFilter::Project => self.project_root.as_ref(),
            JobScopeFilter::Directory => self.working_directory.as_ref(),
            JobScopeFilter::All => None,
        }
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| "All saved projects and directories".to_owned());
        let count = self.filtered_jobs(app).len();
        let mut column = Flex::column()
            .with_cross_axis_alignment(CrossAxisAlignment::Stretch)
            .with_main_axis_size(MainAxisSize::Max)
            .with_spacing(10.)
            .with_child(
                Flex::row()
                    .with_spacing(16.)
                    .with_cross_axis_alignment(CrossAxisAlignment::Center)
                    .with_child(scopes.finish())
                    .with_child(Expanded::new(1., search).finish())
                    .finish(),
            )
            .with_child(statuses.finish())
            .with_child(
                Text::new_inline(
                    scope_text,
                    appearance.ui_font_family(),
                    appearance.ui_font_size() - 1.,
                )
                .with_color(theme.sub_text_color(theme.background()).into())
                .with_clip(ClipConfig::ellipsis())
                .finish(),
            );
        if let Some(error) = &self.error {
            column.add_child(
                Text::new(
                    error.clone(),
                    appearance.ui_font_family(),
                    appearance.ui_font_size(),
                )
                .with_color(theme.ui_error_color().into())
                .finish(),
            );
        }
        column.add_child(Expanded::new(1., self.render_list(app)).finish());
        let mut footer = Flex::row()
            .with_cross_axis_alignment(CrossAxisAlignment::Center)
            .with_main_axis_alignment(MainAxisAlignment::SpaceBetween)
            .with_child(
                Text::new_inline(
                    format!("{} of {count} jobs", self.display_limit.min(count)),
                    appearance.ui_font_family(),
                    appearance.ui_font_size() - 1.,
                )
                .with_color(theme.sub_text_color(theme.background()).into())
                .finish(),
            );
        if count > self.display_limit {
            footer.add_child(ChildView::new(&self.more_button).finish());
        }
        column.add_child(footer.finish());
        column.finish()
    }
}

impl TypedActionView for BackgroundJobsView {
    type Action = BackgroundJobsAction;

    fn handle_action(&mut self, action: &Self::Action, ctx: &mut ViewContext<Self>) {
        match action {
            BackgroundJobsAction::SetScope(scope) => {
                self.scope = *scope;
                self.selected = None;
                self.display_limit = 100;
                self.synchronize(ctx);
            }
            BackgroundJobsAction::SetStatus(status) => {
                self.status = status.clone();
                self.selected = None;
                self.display_limit = 100;
                self.synchronize(ctx);
            }
            BackgroundJobsAction::Select(id) => {
                self.selected = Some(*id);
                self.logs_open = true;
                self.logs_scroll = Default::default();
                self.command_scroll = Default::default();
                self.synchronize(ctx);
                ctx.focus(&self.logs);
            }
            BackgroundJobsAction::ViewLogs(id) => {
                self.selected = Some(*id);
                self.logs_open = true;
                self.logs_scroll = Default::default();
                self.command_scroll = Default::default();
                self.synchronize(ctx);
                ctx.focus(&self.logs);
            }
            BackgroundJobsAction::Back => {
                self.logs_open = false;
                self.synchronize(ctx);
                ctx.focus(&self.search);
            }
            BackgroundJobsAction::Cancel(id) => {
                self.error = BackgroundJobsModel::handle(ctx)
                    .update(ctx, |model, ctx| model.cancel(*id, ctx))
                    .err();
                self.synchronize(ctx);
            }
            BackgroundJobsAction::CancelSelected => {
                if let Some(id) = self.selected {
                    self.error = BackgroundJobsModel::handle(ctx)
                        .update(ctx, |model, ctx| model.cancel(id, ctx))
                        .err();
                    self.synchronize(ctx);
                }
            }
            BackgroundJobsAction::ReloadLogs => self.load_selected_logs(ctx),
            BackgroundJobsAction::LoadMore => {
                self.display_limit = self.display_limit.saturating_add(100);
                self.synchronize(ctx);
            }
        }
    }
}

#[cfg(test)]
#[path = "view_tests.rs"]
mod tests;

#[cfg(test)]
mod remote_scope_tests {
    use super::*;

    #[test]
    fn remote_windows_paths_preserve_drives_and_match_verbatim_paths() {
        assert!(scope_paths_match(
            Path::new(r"\\?\C:\repo\server"),
            Path::new("C:/repo/server"),
            true,
        ));
        assert!(scope_paths_match(
            Path::new(r"\\?\UNC\host\share\repo"),
            Path::new(r"\\host\share\repo"),
            true,
        ));
        assert!(!scope_paths_match(
            Path::new(r"C:\repo"),
            Path::new(r"D:\repo"),
            true,
        ));
        assert!(!scope_paths_match(
            Path::new(r"C:\repo"),
            Path::new("/repo"),
            true,
        ));
    }
}
