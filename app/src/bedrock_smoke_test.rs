use std::time::Duration;

use galaxyui::r#async::Timer;
use galaxyui::ViewHandle;
use galaxyui::WindowId;

use crate::ai::agent::{AIAgentOutputStatus, AIAgentTextSection, FinishedAIAgentOutput};
use crate::pane_group::PaneGroup;
use crate::terminal::TerminalView;
use crate::workspace::Workspace;
use crate::BlocklistAIHistoryModel;

const TARGET_DIR: &str = "~/GIT/stitcher/stitcher";

const FILE_VISIBILITY_QUERY: &str = r#"/agent Okay, let's try again. Can you see files now? List the files in the current directory using your tools. Then respond with EXACTLY this structured format:

results: yes
file_count: <number of files/directories you found>

If you cannot see any files or your tools fail, respond with:

results: no
file_count: 0

You MUST use the file_glob or run_shell_command tool to check the directory contents first, then provide the structured response."#;

#[allow(dead_code)]
const AGENT_QUERY: &str = r#"/agent Analyze this project and respond with EXACTLY this structured format at the end of your response:

Answer: <a 2-3 sentence summary of how this project handles video uploads and media conversions>
ProjectDescription: <a 1 sentence description of what this project is>

You MUST include both "Answer:" and "ProjectDescription:" fields in your final response. Use tools to explore the codebase first, then provide your structured answer."#;

const INITIAL_DELAY: Duration = Duration::from_secs(5);
const CD_SETTLE_DELAY: Duration = Duration::from_secs(3);
const POLL_INTERVAL: Duration = Duration::from_millis(2000);
const MAX_WAIT: Duration = Duration::from_secs(300);

pub fn schedule(ctx: &mut galaxyui::ViewContext<Workspace>) {
    log::info!("[smoke-test] Bedrock smoke test scheduled — starting in {:?}", INITIAL_DELAY);

    ctx.spawn(
        async move { Timer::after(INITIAL_DELAY).await },
        |_ws: &mut Workspace, _, ctx| {
            let Some(window_id) = ctx.window_ids().next() else {
                log::error!("[smoke-test] No window, aborting");
                std::process::exit(1);
            };
            run_cd(ctx, window_id);
        },
    );
}

fn run_cd(ctx: &mut galaxyui::ViewContext<Workspace>, window_id: WindowId) {
    log::info!("[smoke-test] cd {TARGET_DIR}");

    let terminal_view = get_terminal_view(ctx, window_id);
    terminal_view.update(ctx, |view, ctx| {
        view.write_to_pty(format!("cd {TARGET_DIR}\n").into_bytes(), ctx);
    });

    ctx.spawn(
        async move { Timer::after(CD_SETTLE_DELAY).await },
        move |_ws: &mut Workspace, _, ctx| {
            submit_file_visibility_test(ctx, window_id);
        },
    );
}

fn submit_file_visibility_test(ctx: &mut galaxyui::ViewContext<Workspace>, window_id: WindowId) {
    log::info!("[smoke-test] === FILE VISIBILITY TEST ===");
    log::info!("[smoke-test] Submitting file visibility query...");

    let terminal_view = get_terminal_view(ctx, window_id);
    terminal_view.update(ctx, |view, ctx| {
        let input = view.input().clone();
        input.update(ctx, |input, ctx| {
            input.submit_queued_prompt(FILE_VISIBILITY_QUERY.to_string(), ctx);
        });
    });

    poll_file_visibility(ctx, window_id, std::time::Instant::now());
}

fn poll_file_visibility(
    ctx: &mut galaxyui::ViewContext<Workspace>,
    window_id: WindowId,
    start: std::time::Instant,
) {
    ctx.spawn(
        async move { Timer::after(POLL_INTERVAL).await },
        move |_ws: &mut Workspace, _, ctx| {
            let elapsed = start.elapsed();
            if elapsed > MAX_WAIT {
                log::error!("[smoke-test] FILE VISIBILITY TEST TIMEOUT after {:?}", elapsed);
                std::process::exit(1);
            }

            let terminal_view = get_terminal_view(ctx, window_id);
            if let Some(full_text) = get_finished_text(ctx, &terminal_view) {
                log::info!("[smoke-test] === FILE VISIBILITY RESULT ({:.1}s) ===", elapsed.as_secs_f64());

                let results = extract_field(&full_text, "results:");
                let file_count = extract_field(&full_text, "file_count:");

                match (&results, &file_count) {
                    (Some(r), Some(c)) => {
                        log::info!("[smoke-test] results: {}", r);
                        log::info!("[smoke-test] file_count: {}", c);

                        if *r == "yes" {
                            let count: u32 = c.parse().unwrap_or(0);
                            if count > 0 {
                                log::info!("[smoke-test] === FILE VISIBILITY TEST PASSED (found {} files) ===", count);
                                log::info!("[smoke-test] LLM can see files. Proceeding to main test...");
                                std::process::exit(0);
                            } else {
                                log::error!("[smoke-test] === FILE VISIBILITY TEST FAILED (results=yes but file_count=0) ===");
                                std::process::exit(1);
                            }
                        } else {
                            log::error!("[smoke-test] === FILE VISIBILITY TEST FAILED (results=no) ===");
                            log::error!("[smoke-test] The LLM cannot see files in {}", TARGET_DIR);
                            log::error!("[smoke-test] Full response:");
                            for line in full_text.lines() {
                                log::error!("[smoke-test]   {}", line);
                            }
                            std::process::exit(1);
                        }
                    }
                    _ => {
                        log::error!("[smoke-test] === FILE VISIBILITY TEST FAILED (missing structured fields) ===");
                        log::error!("[smoke-test] Full response:");
                        for line in full_text.lines() {
                            log::error!("[smoke-test]   {}", line);
                        }
                        std::process::exit(1);
                    }
                }
            }

            poll_file_visibility(ctx, window_id, start);
        },
    );
}

#[allow(dead_code)]
fn submit_query(ctx: &mut galaxyui::ViewContext<Workspace>, window_id: WindowId) {
    log::info!("[smoke-test] Submitting query...");

    let terminal_view = get_terminal_view(ctx, window_id);
    terminal_view.update(ctx, |view, ctx| {
        let input = view.input().clone();
        input.update(ctx, |input, ctx| {
            input.submit_queued_prompt(AGENT_QUERY.to_string(), ctx);
        });
    });

    log::info!("[smoke-test] Polling for completion (max {:?})", MAX_WAIT);
    poll(ctx, window_id, std::time::Instant::now());
}

#[allow(dead_code)]
fn poll(
    ctx: &mut galaxyui::ViewContext<Workspace>,
    window_id: WindowId,
    start: std::time::Instant,
) {
    ctx.spawn(
        async move { Timer::after(POLL_INTERVAL).await },
        move |_ws: &mut Workspace, _, ctx| {
            let elapsed = start.elapsed();
            if elapsed > MAX_WAIT {
                log::error!("[smoke-test] TIMEOUT after {:?}", elapsed);
                std::process::exit(1);
            }

            let terminal_view = get_terminal_view(ctx, window_id);
            if let Some(full_text) = get_finished_text(ctx, &terminal_view) {
                log::info!("[smoke-test] === CONVERSATION COMPLETE ({:.1}s) ===", elapsed.as_secs_f64());
                log::info!("[smoke-test] Full response length: {} chars", full_text.len());

                let answer = extract_field(&full_text, "Answer:");
                let project_desc = extract_field(&full_text, "ProjectDescription:");

                match (&answer, &project_desc) {
                    (Some(a), Some(p)) => {
                        log::info!("[smoke-test] ========================================");
                        log::info!("[smoke-test] Answer: {}", a);
                        log::info!("[smoke-test] ProjectDescription: {}", p);
                        log::info!("[smoke-test] ========================================");
                        log::info!("[smoke-test] === TEST PASSED ===");
                        std::process::exit(0);
                    }
                    _ => {
                        log::warn!("[smoke-test] ========================================");
                        if let Some(a) = &answer {
                            log::info!("[smoke-test] Answer: {}", a);
                        } else {
                            log::warn!("[smoke-test] MISSING: Answer field not found in response");
                        }
                        if let Some(p) = &project_desc {
                            log::info!("[smoke-test] ProjectDescription: {}", p);
                        } else {
                            log::warn!("[smoke-test] MISSING: ProjectDescription field not found in response");
                        }
                        log::warn!("[smoke-test] ========================================");
                        log::warn!("[smoke-test] Full text dump:");
                        for line in full_text.lines() {
                            log::warn!("[smoke-test]   {}", line);
                        }
                        log::warn!("[smoke-test] === TEST FAILED (missing structured fields) ===");
                        std::process::exit(1);
                    }
                }
            }

            poll(ctx, window_id, start);
        },
    );
}

fn extract_field<'a>(text: &'a str, prefix: &str) -> Option<&'a str> {
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with(prefix) {
            let value = trimmed[prefix.len()..].trim();
            if !value.is_empty() {
                return Some(value);
            }
        }
    }
    None
}

fn get_finished_text(
    ctx: &galaxyui::ViewContext<Workspace>,
    terminal_view: &ViewHandle<TerminalView>,
) -> Option<String> {
    use galaxyui::SingletonEntity;

    let view_id = terminal_view.id();
    BlocklistAIHistoryModel::handle(ctx).read(ctx, |history, _| {
        let conv = history.active_conversation(view_id)?;

        if conv.status().is_in_progress() {
            log::info!("[smoke-test] Still running...");
            return None;
        }

        let mut all_text = String::new();
        for exchange in conv.exchanges_reversed() {
            match &exchange.output_status {
                AIAgentOutputStatus::Finished {
                    finished_output: FinishedAIAgentOutput::Success { output },
                } => {
                    let output = output.get();
                    for text_section in output.text_from_agent_output() {
                        for section in &text_section.sections {
                            match section {
                                AIAgentTextSection::PlainText { text } => {
                                    all_text.push_str(text.text());
                                    all_text.push('\n');
                                }
                                AIAgentTextSection::Code { code, .. } => {
                                    all_text.push_str(code);
                                    all_text.push('\n');
                                }
                                _ => {}
                            }
                        }
                    }
                }
                AIAgentOutputStatus::Finished {
                    finished_output: FinishedAIAgentOutput::Error { error, .. },
                } => {
                    log::error!("[smoke-test] Conversation finished with error: {error}");
                    return None;
                }
                AIAgentOutputStatus::Finished {
                    finished_output: FinishedAIAgentOutput::Cancelled { .. },
                } => {
                    log::error!("[smoke-test] Conversation was cancelled");
                    return None;
                }
                _ => {
                    log::info!("[smoke-test] Exchange still in progress...");
                    return None;
                }
            }
        }

        if all_text.is_empty() {
            log::warn!("[smoke-test] Conversation finished but no text output found");
            return None;
        }

        Some(all_text)
    })
}

fn get_terminal_view(
    ctx: &galaxyui::ViewContext<Workspace>,
    window_id: WindowId,
) -> ViewHandle<TerminalView> {
    let pane_group: ViewHandle<PaneGroup> = ctx
        .views_of_type(window_id)
        .expect("[smoke-test] views for window")
        .first()
        .expect("[smoke-test] pane group")
        .clone();

    pane_group.read(ctx, |pg, ctx| {
        pg.terminal_views(ctx)
            .into_iter()
            .next()
            .expect("[smoke-test] should have at least one terminal view")
    })
}
