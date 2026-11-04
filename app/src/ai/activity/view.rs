//! App-wide live browser for local AI diagnostics.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use galaxyui::elements::{
    Border, ChildView, Clipped, ClippedScrollStateHandle, ClippedScrollable, ConstrainedBox,
    Container, CrossAxisAlignment, Expanded, Flex, Hoverable, MainAxisAlignment, MainAxisSize,
    MouseStateHandle, Padding, ParentElement, RowBackground, ScrollbarWidth, Table,
    TableColumnWidth, TableConfig, TableHeader, TableStateHandle, TableVerticalSizing, Text,
};
use galaxyui::fonts::{Properties, Weight};
use galaxyui::platform::Cursor;
use galaxyui::r#async::Timer;
use galaxyui::text_layout::ClipConfig;
use galaxyui::{
    AppContext, Element, Entity, SingletonEntity, TypedActionView, View, ViewContext, ViewHandle,
};

use super::{ActivityEntry, ActivitySnapshot};
use crate::appearance::Appearance;
use crate::editor::{
    EditorOptions, EditorView, Event as EditorEvent, InteractionState, PlainTextEditorViewAction,
    SingleLineEditorOptions, TextOptions,
};
use crate::modal::ModalAction;
use crate::view_components::action_button::{ActionButton, ButtonSize, NakedTheme, SecondaryTheme};

const PAGE_SIZE: usize = 200;

#[derive(Clone, Debug)]
pub enum AIActivityAction {
    ToggleRecording,
    Clear,
    Select(u64),
    FilterSession(Option<String>),
    FilterCategory(Option<String>),
    ShowMore,
}

pub struct AIActivityView {
    snapshot: ActivitySnapshot,
    search: ViewHandle<EditorView>,
    detail: ViewHandle<EditorView>,
    recording_button: ViewHandle<ActionButton>,
    clear_button: ViewHandle<ActionButton>,
    all_sessions_button: ViewHandle<ActionButton>,
    more_button: ViewHandle<ActionButton>,
    category_buttons: Vec<(Option<String>, ViewHandle<ActionButton>)>,
    selected: Option<Arc<ActivityEntry>>,
    session_filter: Option<String>,
    category_filter: Option<String>,
    rows: HashMap<u64, (MouseStateHandle, MouseStateHandle)>,
    display_limit: usize,
    visible: bool,
    refresh_scheduled: bool,
    list_scroll: ClippedScrollStateHandle,
    detail_scroll: ClippedScrollStateHandle,
    filters_scroll: ClippedScrollStateHandle,
    table_state: TableStateHandle,
}

impl AIActivityView {
    pub fn new(ctx: &mut ViewContext<Self>) -> Self {
        let search = ctx.add_typed_action_view(|ctx| {
            let mut editor = EditorView::single_line(SingleLineEditorOptions::default(), ctx);
            editor.set_placeholder_text("Search activity, model, or session", ctx);
            editor
        });
        ctx.subscribe_to_view(&search, |me, _, event, ctx| match event {
            EditorEvent::Edited(_) => {
                me.display_limit = PAGE_SIZE;
                me.synchronize_rows(ctx);
            }
            EditorEvent::Escape => ctx.dispatch_typed_action(&ModalAction::Close),
            _ => {}
        });
        let appearance = Appearance::as_ref(ctx);
        let font = appearance.monospace_font_family();
        let size = appearance.monospace_font_size();
        let detail = ctx.add_typed_action_view(|ctx| {
            let mut editor = EditorView::new(
                EditorOptions {
                    text: TextOptions {
                        font_family_override: Some(font),
                        font_size_override: Some(size),
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
        ctx.subscribe_to_view(&detail, |_, _, event, ctx| {
            if matches!(event, EditorEvent::Escape) {
                ctx.dispatch_typed_action(&ModalAction::Close);
            }
        });
        let recording_button = ctx.add_typed_action_view(|_| {
            ActionButton::new("Pause recording", SecondaryTheme)
                .with_size(ButtonSize::Small)
                .on_click(|ctx| ctx.dispatch_typed_action(AIActivityAction::ToggleRecording))
        });
        let clear_button = ctx.add_typed_action_view(|_| {
            ActionButton::new("Clear", SecondaryTheme)
                .with_size(ButtonSize::Small)
                .on_click(|ctx| ctx.dispatch_typed_action(AIActivityAction::Clear))
        });
        let all_sessions_button = ctx.add_typed_action_view(|_| {
            ActionButton::new("All sessions", NakedTheme)
                .with_size(ButtonSize::Small)
                .on_click(|ctx| ctx.dispatch_typed_action(AIActivityAction::FilterSession(None)))
        });
        let more_button = ctx.add_typed_action_view(|_| {
            ActionButton::new("Show more", SecondaryTheme)
                .with_size(ButtonSize::Small)
                .on_click(|ctx| ctx.dispatch_typed_action(AIActivityAction::ShowMore))
        });
        let category_buttons = [
            (None, "All activity"),
            (Some("Completions"), "Models"),
            (Some("Compaction"), "Compaction"),
            (Some("Tools"), "Tools"),
            (Some("Permissions"), "Permissions"),
            (Some("Background jobs"), "Jobs"),
            (Some("Lifecycle"), "Lifecycle"),
        ]
        .into_iter()
        .map(|(category, label)| {
            let category = category.map(str::to_owned);
            let value = category.clone();
            let button = ctx.add_typed_action_view(|_| {
                ActionButton::new(label, NakedTheme)
                    .with_size(ButtonSize::Small)
                    .on_click(move |ctx| {
                        ctx.dispatch_typed_action(AIActivityAction::FilterCategory(value.clone()))
                    })
            });
            (category, button)
        })
        .collect();
        Self {
            // Workspace modals are constructed while hidden. Do not retain global
            // payloads until this inspector is actually opened.
            snapshot: ActivitySnapshot {
                recording: super::is_recording(),
                revision: 0,
                dropped: 0,
                entries: Vec::new(),
            },
            search,
            detail,
            recording_button,
            clear_button,
            all_sessions_button,
            more_button,
            category_buttons,
            selected: None,
            session_filter: None,
            category_filter: None,
            rows: HashMap::new(),
            display_limit: PAGE_SIZE,
            visible: false,
            refresh_scheduled: false,
            list_scroll: Default::default(),
            detail_scroll: Default::default(),
            filters_scroll: Default::default(),
            table_state: Default::default(),
        }
    }

    pub fn on_open(&mut self, ctx: &mut ViewContext<Self>) {
        // Opening the inspector is the explicit opt-in to in-memory payload capture.
        if super::snapshot().revision == 0 {
            super::set_recording(true);
        }
        self.visible = true;
        self.refresh(ctx);
        self.update_recording_button(ctx);
        self.synchronize_rows(ctx);
        self.schedule_refresh(ctx);
        ctx.focus(&self.search);
    }

    pub fn on_close(&mut self, ctx: &mut ViewContext<Self>) {
        self.visible = false;
        self.snapshot.entries.clear();
        self.rows.clear();
        self.clear_detail(ctx);
        self.list_scroll = Default::default();
        self.table_state = Default::default();
        // Leave any pending refresh scheduled: it will observe `visible`, or
        // resume on a rapid reopen, without creating a second polling chain.
        ctx.notify();
    }

    fn schedule_refresh(&mut self, ctx: &mut ViewContext<Self>) {
        if self.refresh_scheduled || !self.visible {
            return;
        }
        self.refresh_scheduled = true;
        ctx.spawn(
            async { Timer::after(Duration::from_millis(500)).await },
            |me, _, ctx| {
                me.refresh_scheduled = false;
                if me.visible {
                    me.refresh(ctx);
                    me.schedule_refresh(ctx);
                }
            },
        );
    }

    fn refresh(&mut self, ctx: &mut ViewContext<Self>) {
        let snapshot = super::snapshot();
        let changed = snapshot.revision != self.snapshot.revision
            || snapshot.recording != self.snapshot.recording;
        self.snapshot = snapshot;
        if changed {
            if self.selected.as_ref().is_some_and(|selected| {
                !self
                    .snapshot
                    .entries
                    .iter()
                    .any(|entry| entry.id == selected.id)
            }) {
                self.clear_detail(ctx);
            }
            self.update_recording_button(ctx);
            self.synchronize_rows(ctx);
        }
    }

    fn clear_detail(&mut self, ctx: &mut ViewContext<Self>) {
        self.selected = None;
        self.detail.update(ctx, |editor, ctx| {
            editor.set_interaction_state(InteractionState::Editable, ctx);
            editor.clear_buffer_and_reset_undo_stack(ctx);
            editor.set_interaction_state(InteractionState::Selectable, ctx);
        });
        self.detail_scroll = Default::default();
    }

    fn update_recording_button(&self, ctx: &mut ViewContext<Self>) {
        self.recording_button.update(ctx, |button, ctx| {
            button.set_label(
                if self.snapshot.recording {
                    "Pause recording"
                } else {
                    "Record"
                },
                ctx,
            );
        });
    }

    fn filtered_entries(&self, app: &AppContext) -> Vec<Arc<ActivityEntry>> {
        let query = self
            .search
            .as_ref(app)
            .buffer_text(app)
            .trim()
            .to_lowercase();
        self.snapshot
            .entries
            .iter()
            .rev()
            .filter(|entry| {
                self.session_filter
                    .as_ref()
                    .is_none_or(|session| entry.session_id.as_ref() == Some(session))
                    && self
                        .category_filter
                        .as_ref()
                        .is_none_or(|category| entry.category == *category)
                    && (query.is_empty()
                        || [
                            &entry.session_label,
                            &entry.summary,
                            &entry.event,
                            &entry.category,
                        ]
                        .into_iter()
                        .any(|field| field.to_lowercase().contains(&query))
                        || entry
                            .session_id
                            .as_ref()
                            .is_some_and(|id| id.to_lowercase().contains(&query))
                        || entry
                            .model
                            .as_ref()
                            .is_some_and(|model| model.to_lowercase().contains(&query)))
            })
            .cloned()
            .collect()
    }

    fn synchronize_rows(&mut self, ctx: &mut ViewContext<Self>) {
        let ids = self
            .filtered_entries(ctx)
            .into_iter()
            .take(self.display_limit)
            .map(|entry| entry.id)
            .collect::<HashSet<_>>();
        self.rows.retain(|id, _| ids.contains(id));
        for id in ids {
            self.rows.entry(id).or_default();
        }
        for (category, button) in &self.category_buttons {
            button.update(ctx, |button, ctx| {
                button.set_active(*category == self.category_filter, ctx)
            });
        }
        self.all_sessions_button.update(ctx, |button, ctx| {
            button.set_active(self.session_filter.is_none(), ctx);
        });
        ctx.notify();
    }

    fn select_entry(&mut self, id: u64, ctx: &mut ViewContext<Self>) {
        let Some(entry) = self
            .snapshot
            .entries
            .iter()
            .find(|entry| entry.id == id)
            .cloned()
        else {
            return;
        };
        let session_id = entry.session_id.as_deref().unwrap_or("Unscoped");
        let text = format!(
            "{}\nSession: {}\nSession ID: {}\nTime: {}\nEvent: {}\n\n{}",
            entry.summary,
            entry.session_label,
            session_id,
            entry
                .timestamp
                .with_timezone(&chrono::Local)
                .format("%Y-%m-%d %H:%M:%S%.3f %Z"),
            entry.event,
            readable_detail(&entry),
        );
        self.detail.update(ctx, |editor, ctx| {
            editor.set_interaction_state(InteractionState::Editable, ctx);
            editor.clear_buffer_and_reset_undo_stack(ctx);
            editor.insert_with_styles(&text, &[], PlainTextEditorViewAction::ReplaceBuffer, ctx);
            editor.set_interaction_state(InteractionState::Selectable, ctx);
        });
        self.selected = Some(entry);
        self.detail_scroll = Default::default();
        ctx.notify();
    }

    fn render_table(&self, app: &AppContext) -> Box<dyn Element> {
        let appearance = Appearance::as_ref(app);
        let theme = appearance.theme();
        let entries = self
            .filtered_entries(app)
            .into_iter()
            .take(self.display_limit)
            .collect::<Vec<_>>();
        if entries.is_empty() {
            let message = if self.snapshot.entries.is_empty() {
                "Waiting for AI activity. Continue a conversation in any session to capture requests, responses, tools, and compaction."
            } else {
                "No activity matches these filters."
            };
            return Container::new(
                Text::new(
                    message,
                    appearance.ui_font_family(),
                    appearance.ui_font_size(),
                )
                .soft_wrap(true)
                .with_color(theme.sub_text_color(theme.background()).into())
                .finish(),
            )
            .with_padding(Padding::uniform(20.))
            .finish();
        }
        let headers = [
            ("Time", TableColumnWidth::Fixed(102.)),
            ("Session", TableColumnWidth::Flex(1.)),
            ("Type", TableColumnWidth::Fixed(100.)),
            ("Activity", TableColumnWidth::Flex(1.8)),
            ("Model", TableColumnWidth::Flex(1.)),
            ("Duration", TableColumnWidth::Fixed(88.)),
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
        let selected = self.selected.as_ref().map(|entry| entry.id);
        let table = Table::new(self.table_state.clone(), 0., 0.)
            .with_headers(headers)
            .with_row_count(entries.len())
            .with_row_render_fn(move |index, app| {
                let appearance = Appearance::as_ref(app);
                let theme = appearance.theme();
                let entry = &entries[index];
                let id = entry.id;
                let Some((row_mouse, session_mouse)) = rows.get(&id) else {
                    return Vec::new();
                };
                let text = |value: String| {
                    Text::new_inline(
                        value,
                        appearance.ui_font_family(),
                        appearance.ui_font_size() - 1.,
                    )
                    .with_clip(ClipConfig::ellipsis())
                    .with_color(theme.active_ui_text_color().into())
                    .finish()
                };
                let session_id = entry.session_id.clone();
                let session = text(entry.session_label.clone());
                let session = if session_id.is_some() {
                    Hoverable::new(session_mouse.clone(), move |_| session)
                        .on_click(move |ctx, _, _| {
                            ctx.dispatch_typed_action(AIActivityAction::FilterSession(
                                session_id.clone(),
                            ))
                        })
                        .with_cursor(Cursor::PointingHand)
                        .finish()
                } else {
                    session
                };
                let summary = entry.summary.clone();
                let activity = Hoverable::new(row_mouse.clone(), move |_| {
                    Text::new_inline(
                        summary,
                        appearance.ui_font_family(),
                        appearance.ui_font_size(),
                    )
                    .with_clip(ClipConfig::ellipsis())
                    .with_style(Properties::default().weight(if selected == Some(id) {
                        Weight::Semibold
                    } else {
                        Weight::Normal
                    }))
                    .with_color(theme.active_ui_text_color().into())
                    .finish()
                })
                .on_click(move |ctx, _, _| ctx.dispatch_typed_action(AIActivityAction::Select(id)))
                .with_cursor(Cursor::PointingHand)
                .finish();
                vec![
                    text(
                        entry
                            .timestamp
                            .with_timezone(&chrono::Local)
                            .format("%H:%M:%S%.3f")
                            .to_string(),
                    ),
                    session,
                    text(entry.category.clone()),
                    activity,
                    text(entry.model.clone().unwrap_or_else(|| "—".to_owned())),
                    text(
                        entry
                            .duration_ms
                            .map(|ms| format!("{:.2} s", ms as f64 / 1000.))
                            .unwrap_or_else(|| "—".to_owned()),
                    ),
                ]
            })
            .with_config(TableConfig {
                border_width: 1.,
                border_color: theme.outline().into(),
                outer_border: true,
                column_dividers: false,
                row_dividers: true,
                cell_padding: 10.,
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
}

/// Legacy/ACP and projected tool arguments arrive as bounded Debug strings.
/// Unwrap those strings for reading while retaining their operational metadata.
/// A truncated capture is deliberately left as text rather than parsed as JSON.
fn readable_detail(entry: &ActivityEntry) -> String {
    let Ok(capture) = serde_json::from_str::<serde_json::Value>(&entry.detail) else {
        return entry.detail.clone();
    };
    let payload = &capture["payload"];
    let raw = if entry.event == "tool_call" {
        payload.as_str()
    } else if matches!(
        entry.event.as_str(),
        "raw_model_request" | "raw_model_response"
    ) {
        payload
            .pointer("/raw_payload/payload")
            .and_then(serde_json::Value::as_str)
    } else {
        None
    };
    let Some(raw) = raw else {
        return entry.detail.clone();
    };
    let decoded = serde_json::from_str::<String>(raw).unwrap_or_else(|_| raw.to_owned());
    let mut metadata = capture;
    if entry.event == "tool_call" {
        metadata["payload"] = serde_json::json!("See captured content below");
    } else if let Some(raw_payload) = metadata
        .pointer_mut("/payload/raw_payload")
        .and_then(serde_json::Value::as_object_mut)
    {
        raw_payload.remove("payload");
    }
    format!(
        "{}\n\nCaptured content:\n{decoded}",
        serde_json::to_string_pretty(&metadata).unwrap_or_default()
    )
}

impl Entity for AIActivityView {
    type Event = ();
}

impl View for AIActivityView {
    fn ui_name() -> &'static str {
        "AIActivityView"
    }

    fn render(&self, app: &AppContext) -> Box<dyn Element> {
        let appearance = Appearance::as_ref(app);
        let theme = appearance.theme();
        let small_text = |text: String| {
            Text::new(
                text,
                appearance.ui_font_family(),
                appearance.ui_font_size() - 1.,
            )
            .soft_wrap(true)
            .with_color(theme.sub_text_color(theme.background()).into())
            .finish()
        };
        let search = Container::new(Clipped::new(ChildView::new(&self.search).finish()).finish())
            .with_horizontal_padding(10.)
            .with_vertical_padding(8.)
            .with_border(Border::all(1.).with_border_color(theme.outline().into()))
            .finish();
        let mut filters = Flex::row()
            .with_spacing(4.)
            .with_cross_axis_alignment(CrossAxisAlignment::Center);
        for (_, button) in &self.category_buttons {
            filters.add_child(ChildView::new(button).finish());
        }
        filters.add_child(ChildView::new(&self.all_sessions_button).finish());
        let filters = ConstrainedBox::new(
            ClippedScrollable::horizontal(
                self.filters_scroll.clone(),
                filters.finish(),
                ScrollbarWidth::Auto,
                theme.nonactive_ui_detail().into(),
                theme.active_ui_detail().into(),
                galaxyui::elements::Fill::None,
            )
            .finish(),
        )
        .with_height(36.)
        .finish();
        let session_label = self.session_filter.as_ref().map(|id| {
            self.snapshot
                .entries
                .iter()
                .find(|entry| entry.session_id.as_ref() == Some(id))
                .map(|entry| entry.session_label.clone())
                .unwrap_or_else(|| id.clone())
        });
        let count = self.filtered_entries(app).len();
        let mut footer = Flex::row()
            .with_main_axis_alignment(MainAxisAlignment::SpaceBetween)
            .with_cross_axis_alignment(CrossAxisAlignment::Center)
            .with_child(small_text(format!(
                "{} of {count} events · {}{}",
                self.display_limit.min(count),
                session_label
                    .map(|label| format!("Session: {label}"))
                    .unwrap_or_else(|| "All sessions · click a session to filter".to_owned()),
                if self.snapshot.dropped > 0 {
                    format!(" · {} older events discarded", self.snapshot.dropped)
                } else {
                    String::new()
                }
            )));
        if count > self.display_limit {
            footer.add_child(ChildView::new(&self.more_button).finish());
        }
        let detail = if self.selected.is_some() {
            ClippedScrollable::vertical(
                self.detail_scroll.clone(),
                Container::new(ChildView::new(&self.detail).finish())
                    .with_padding(Padding::uniform(12.))
                    .finish(),
                ScrollbarWidth::Auto,
                theme.nonactive_ui_detail().into(),
                theme.active_ui_detail().into(),
                galaxyui::elements::Fill::None,
            )
            .finish()
        } else {
            Container::new(small_text("Click an activity to inspect its request, response, or lifecycle details. Text is selectable and wrapped.".to_owned()))
                .with_padding(Padding::uniform(16.)).finish()
        };
        Flex::column().with_cross_axis_alignment(CrossAxisAlignment::Stretch)
            .with_main_axis_size(MainAxisSize::Max).with_spacing(10.)
            .with_child(Flex::row().with_spacing(8.).with_cross_axis_alignment(CrossAxisAlignment::Center)
                .with_child(Expanded::new(1., search).finish())
                .with_child(ChildView::new(&self.recording_button).finish())
                .with_child(ChildView::new(&self.clear_button).finish()).finish())
            .with_child(filters)
            .with_child(small_text(format!("{} · Local Galaxy request/result snapshots, up to 2,000 events / 16 MiB and 256 Ki characters per event. Recording continues when this window closes.",
                if self.snapshot.recording { "Recording all sessions" } else { "Recording paused" })))
            .with_child(Expanded::new(1., self.render_table(app)).finish())
            .with_child(footer.finish())
            .with_child(Expanded::new(1., Container::new(detail)
                .with_background(theme.background())
                .with_border(Border::all(1.).with_border_color(theme.outline().into())).finish()).finish())
            .finish()
    }
}

impl TypedActionView for AIActivityView {
    type Action = AIActivityAction;

    fn handle_action(&mut self, action: &Self::Action, ctx: &mut ViewContext<Self>) {
        match action {
            AIActivityAction::ToggleRecording => {
                super::set_recording(!super::snapshot().recording);
                self.refresh(ctx);
            }
            AIActivityAction::Clear => {
                super::clear();
                self.clear_detail(ctx);
                self.refresh(ctx);
            }
            AIActivityAction::Select(id) => self.select_entry(*id, ctx),
            AIActivityAction::FilterSession(session) => {
                self.session_filter = session.clone();
                self.display_limit = PAGE_SIZE;
                self.list_scroll = Default::default();
                self.synchronize_rows(ctx);
            }
            AIActivityAction::FilterCategory(category) => {
                self.category_filter = category.clone();
                self.display_limit = PAGE_SIZE;
                self.list_scroll = Default::default();
                self.synchronize_rows(ctx);
            }
            AIActivityAction::ShowMore => {
                self.display_limit += PAGE_SIZE;
                self.synchronize_rows(ctx);
            }
        }
    }
}
