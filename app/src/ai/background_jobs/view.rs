//! The project-scoped history and log browser for agent-started background commands.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use std::time::{Duration, Instant};

use galaxy_core::ui::icons::Icon;
use galaxy_core::ui::theme::color::internal_colors;
use galaxy_core::HostId;
use galaxy_util::local_or_remote_path::LocalOrRemotePath;
use galaxy_util::standardized_path::StandardizedPath;
use galaxyui::elements::{
    Border, ChildView, Clipped, ClippedScrollStateHandle, ClippedScrollable, ConstrainedBox,
    Container, CrossAxisAlignment, Flex, Hoverable, MainAxisAlignment, MouseStateHandle, Padding,
    ParentElement, Point, ScrollbarWidth, Shrinkable, Text,
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
    EditorOptions, EditorView, Event as EditorEvent, InteractionState, SingleLineEditorOptions,
    TextOptions,
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
    logs_scroll: ClippedScrollStateHandle,
    displayed_logs: String,
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
        let logs_size = Appearance::as_ref(ctx).ui_font_size();
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
                    ..Default::default()
                },
                ctx,
            );
            editor.set_interaction_state(InteractionState::Selectable, ctx);
            editor
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
            logs_scroll: Default::default(),
            displayed_logs: String::new(),
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
        self.logs_open = false;
        self.visible = true;
        self.display_limit = 100;
        self.error = None;
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
        ctx.focus(&self.search);
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
        if text != self.displayed_logs {
            self.logs.update(ctx, |editor, ctx| {
                editor.set_buffer_text_ignoring_undo(&text, ctx)
            });
            self.displayed_logs = text;
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
        let mut list = Flex::column().with_cross_axis_alignment(CrossAxisAlignment::Stretch);
        let jobs = self.filtered_jobs(app);
        if jobs.is_empty() {
            list.add_child(
                Container::new(
                    Text::new_inline(
                        "No background jobs match these filters.",
                        appearance.ui_font_family(),
                        appearance.ui_font_size(),
                    )
                    .with_color(theme.sub_text_color(theme.background()).into())
                    .finish(),
                )
                .with_padding(Padding::uniform(16.))
                .finish(),
            );
        }
        for id in jobs.into_iter().take(self.display_limit) {
            let Some(job) = BackgroundJobsModel::as_ref(app).job(id) else {
                continue;
            };
            let selected = self.selected == Some(job.id);
            let mut row = Flex::row()
                .with_cross_axis_alignment(CrossAxisAlignment::Center)
                .with_child(
                    Container::new(status_icon(&job.status, app))
                        .with_margin_right(8.)
                        .finish(),
                );
            let command = job
                .command
                .lines()
                .next()
                .unwrap_or(&job.command)
                .to_owned();
            let mut text = Flex::column()
                .with_cross_axis_alignment(CrossAxisAlignment::Stretch)
                .with_child(
                    Text::new_inline(
                        command,
                        appearance.monospace_font_family(),
                        appearance.ui_font_size(),
                    )
                    .with_color(theme.active_ui_text_color().into())
                    .with_clip(ClipConfig::ellipsis())
                    .finish(),
                );
            let location = match &job.host_id {
                Some(host) => format!("{host}: {}", job.working_directory.display()),
                None => job.working_directory.display().to_string(),
            };
            text.add_child(
                Text::new_inline(
                    location,
                    appearance.ui_font_family(),
                    appearance.ui_font_size() - 2.,
                )
                .with_color(theme.sub_text_color(theme.background()).into())
                .with_clip(ClipConfig::ellipsis())
                .finish(),
            );
            row.add_child(Shrinkable::new(1., text.finish()).finish());
            row.add_child(
                ConstrainedBox::new(
                    Text::new_inline(
                        job.started_at
                            .with_timezone(&chrono::Local)
                            .format("%b %d, %H:%M")
                            .to_string(),
                        appearance.ui_font_family(),
                        appearance.ui_font_size() - 2.,
                    )
                    .with_color(theme.sub_text_color(theme.background()).into())
                    .finish(),
                )
                .with_width(120.)
                .finish(),
            );
            if let Some(controls) = self.rows.get(&id) {
                row.add_child(
                    ConstrainedBox::new(
                        Text::new_inline(
                            status_label(&job),
                            appearance.ui_font_family(),
                            appearance.ui_font_size() - 1.,
                        )
                        .with_color(theme.sub_text_color(theme.background()).into())
                        .with_clip(ClipConfig::ellipsis())
                        .finish(),
                    )
                    .with_width(135.)
                    .finish(),
                );
                let mut actions = Flex::row().with_spacing(4.);
                if controls.logs_available && BackgroundJobsModel::as_ref(app).logs_available(id) {
                    actions.add_child(ChildView::new(&controls.view_logs).finish());
                }
                if job.status == BackgroundJobStatus::Running {
                    actions.add_child(ChildView::new(&controls.terminate).finish());
                }
                row.add_child(
                    ConstrainedBox::new(actions.finish())
                        .with_width(180.)
                        .finish(),
                );
                let background = if selected {
                    theme.surface_overlay_2()
                } else {
                    theme.surface_2()
                };
                list.add_child(
                    Hoverable::new(controls.mouse.clone(), move |_| {
                        Container::new(row.finish())
                            .with_padding(Padding::uniform(12.))
                            .with_background(background)
                            .finish()
                    })
                    .on_click(move |ctx, _, _| {
                        ctx.dispatch_typed_action(BackgroundJobsAction::Select(id))
                    })
                    .with_cursor(Cursor::PointingHand)
                    .finish(),
                );
            }
        }
        ClippedScrollable::vertical(
            self.list_scroll.clone(),
            list.finish(),
            ScrollbarWidth::Auto,
            theme.nonactive_ui_detail().into(),
            theme.active_ui_detail().into(),
            galaxyui::elements::Fill::None,
        )
        .finish()
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
        let appearance = Appearance::as_ref(app);
        let theme = appearance.theme();
        let mut filters = Flex::row()
            .with_spacing(4.)
            .with_cross_axis_alignment(CrossAxisAlignment::Center);
        for (_, button) in &self.scope_buttons {
            filters.add_child(ChildView::new(button).finish());
        }
        filters.add_child(
            Shrinkable::new(
                1.,
                Container::new(Clipped::new(ChildView::new(&self.search).finish()).finish())
                    .with_horizontal_padding(8.)
                    .with_vertical_padding(6.)
                    .with_border(Border::all(1.).with_border_color(theme.outline().into()))
                    .finish(),
            )
            .finish(),
        );
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
        let mut column = Flex::column()
            .with_cross_axis_alignment(CrossAxisAlignment::Stretch)
            .with_child(filters.finish())
            .with_child(statuses.finish())
            .with_child(
                Container::new(
                    Text::new_inline(
                        scope_text,
                        appearance.ui_font_family(),
                        appearance.ui_font_size() - 2.,
                    )
                    .with_color(theme.sub_text_color(theme.background()).into())
                    .with_clip(ClipConfig::ellipsis())
                    .finish(),
                )
                .with_padding(Padding::uniform(8.))
                .finish(),
            );
        if let Some(error) = &self.error {
            column.add_child(
                Text::new_inline(
                    error.clone(),
                    appearance.ui_font_family(),
                    appearance.ui_font_size(),
                )
                .with_color(theme.ui_error_color().into())
                .finish(),
            );
        }

        if !self.logs_open {
            let label = |text| {
                Text::new_inline(
                    text,
                    appearance.ui_font_family(),
                    appearance.ui_font_size() - 2.,
                )
                .with_color(theme.sub_text_color(theme.background()).into())
                .finish()
            };
            column.add_child(
                Container::new(
                    Flex::row()
                        .with_cross_axis_alignment(CrossAxisAlignment::Center)
                        .with_child(ConstrainedBox::new(label("")).with_width(24.).finish())
                        .with_child(Shrinkable::new(1., label("Command / project")).finish())
                        .with_child(
                            ConstrainedBox::new(label("Started"))
                                .with_width(120.)
                                .finish(),
                        )
                        .with_child(
                            ConstrainedBox::new(label("Status"))
                                .with_width(135.)
                                .finish(),
                        )
                        .with_child(
                            ConstrainedBox::new(label("Actions"))
                                .with_width(180.)
                                .finish(),
                        )
                        .finish(),
                )
                .with_horizontal_padding(12.)
                .with_vertical_padding(6.)
                .finish(),
            );
            column.add_child(
                ConstrainedBox::new(self.render_list(app))
                    .with_height(470.)
                    .finish(),
            );
            let count = self.filtered_jobs(app).len();
            if count > self.display_limit {
                column.add_child(
                    Flex::row()
                        .with_cross_axis_alignment(CrossAxisAlignment::Center)
                        .with_child(ChildView::new(&self.more_button).finish())
                        .with_child(
                            Text::new_inline(
                                format!("Showing {} of {count} jobs", self.display_limit),
                                appearance.ui_font_family(),
                                appearance.ui_font_size() - 2.,
                            )
                            .with_color(theme.sub_text_color(theme.background()).into())
                            .finish(),
                        )
                        .finish(),
                );
            }
            return column.finish();
        }
        let selected = self
            .selected
            .and_then(|id| BackgroundJobsModel::as_ref(app).job(id));
        let mut detail = Flex::column().with_cross_axis_alignment(CrossAxisAlignment::Stretch);
        detail.add_child(ChildView::new(&self.back_button).finish());
        if let Some(job) = selected {
            let status = status_label(job);
            detail.add_child(
                Flex::row()
                    .with_cross_axis_alignment(CrossAxisAlignment::Center)
                    .with_spacing(8.)
                    .with_child(status_icon(&job.status, app))
                    .with_child(
                        Text::new_inline(
                            status,
                            appearance.ui_font_family(),
                            appearance.ui_font_size(),
                        )
                        .with_style(Properties::default().weight(Weight::Semibold))
                        .with_color(theme.active_ui_text_color().into())
                        .finish(),
                    )
                    .finish(),
            );
            detail.add_child(
                Container::new(
                    Text::new_inline(
                        job.command.clone(),
                        appearance.monospace_font_family(),
                        appearance.ui_font_size(),
                    )
                    .with_color(theme.active_ui_text_color().into())
                    .finish(),
                )
                .with_vertical_padding(8.)
                .finish(),
            );
            detail.add_child(
                Text::new_inline(
                    job.working_directory.display().to_string(),
                    appearance.ui_font_family(),
                    appearance.ui_font_size() - 2.,
                )
                .with_color(theme.sub_text_color(theme.background()).into())
                .with_clip(ClipConfig::ellipsis())
                .finish(),
            );
            let mut buttons = Flex::row()
                .with_main_axis_alignment(MainAxisAlignment::End)
                .with_spacing(8.);
            if BackgroundJobsModel::as_ref(app).logs_available(job.id) {
                buttons.add_child(ChildView::new(&self.reload_button).finish());
            }
            if job.status == BackgroundJobStatus::Running {
                buttons.add_child(ChildView::new(&self.cancel_button).finish());
            }
            detail.add_child(
                Container::new(buttons.finish())
                    .with_vertical_padding(8.)
                    .finish(),
            );
            if let Some(error) = &job.error {
                detail.add_child(
                    Text::new_inline(
                        error.clone(),
                        appearance.ui_font_family(),
                        appearance.ui_font_size(),
                    )
                    .with_color(theme.ui_error_color().into())
                    .finish(),
                );
            }
            detail.add_child(
                ConstrainedBox::new(
                    ClippedScrollable::vertical(
                        self.logs_scroll.clone(),
                        ChildView::new(&self.logs).finish(),
                        ScrollbarWidth::Auto,
                        theme.nonactive_ui_detail().into(),
                        theme.active_ui_detail().into(),
                        galaxyui::elements::Fill::None,
                    )
                    .finish(),
                )
                .with_height(340.)
                .finish(),
            );
        } else {
            detail.add_child(
                Text::new_inline(
                    "Select a job to view its command, status, and logs.",
                    appearance.ui_font_family(),
                    appearance.ui_font_size(),
                )
                .with_color(theme.sub_text_color(theme.background()).into())
                .finish(),
            );
        }
        column.add_child(
            Container::new(detail.finish())
                .with_padding(Padding::uniform(12.))
                .finish(),
        );
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
                self.synchronize(ctx);
            }
            BackgroundJobsAction::ViewLogs(id) => {
                self.selected = Some(*id);
                self.logs_open = true;
                self.load_selected_logs(ctx);
                ctx.notify();
            }
            BackgroundJobsAction::Back => {
                self.logs_open = false;
                self.synchronize(ctx);
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
