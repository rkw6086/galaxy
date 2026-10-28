use ai::skills::{ParsedSkill, SkillProvider, SkillReference, SkillScope};
use galaxy_util::local_or_remote_path::LocalOrRemotePath;
use galaxyui::{App, SingletonEntity};

use super::InlineSkillSelectorEvent;
use crate::ai::skills::{SkillManager, SkillManagerEvent};
use crate::terminal::input::slash_command_model::SlashCommandEntryState;
use crate::terminal::input::tests::{add_window_with_bootstrapped_terminal, initialize_app};

#[test]
fn skills_menu_refreshes_after_discovery_and_preserves_the_selected_reference() {
    App::test((), |mut app| async move {
        initialize_app(&mut app);
        let terminal = add_window_with_bootstrapped_terminal(&mut app, None, None).await;
        let input = terminal.read(&app, |terminal, _| terminal.input().clone());
        let selector = input.read(&app, |input, _| input.inline_skill_selector_view.clone());
        let name = "catalog-refresh-test-skill";
        let home = dirs::home_dir().unwrap();
        let first_path =
            LocalOrRemotePath::Local(home.join(format!(".agents/skills/{name}/SKILL.md")));
        let second_path =
            LocalOrRemotePath::Local(home.join(format!(".claude/skills/{name}/SKILL.md")));
        let first = ParsedSkill {
            name: name.to_owned(),
            description: "First installed skill".to_owned(),
            path: first_path,
            content: "First workflow".to_owned(),
            line_range: None,
            provider: SkillProvider::Agents,
            scope: SkillScope::Home,
        };
        let second = ParsedSkill {
            path: second_path.clone(),
            provider: SkillProvider::Claude,
            content: "Second workflow".to_owned(),
            ..first.clone()
        };
        let renamed = ParsedSkill {
            name: "renamed-catalog-refresh-test-skill".to_owned(),
            ..second.clone()
        };

        input.update(&mut app, |input, ctx| {
            input
                .editor
                .update(ctx, |editor, ctx| editor.set_buffer_text("/SKILLS", ctx));
            input.open_invoke_skill_selector(ctx);
        });
        SkillManager::handle(&app).update(&mut app, |manager, ctx| {
            manager.handle_skills_added(vec![first, second]);
            ctx.emit(SkillManagerEvent::SkillsChanged);
        });
        selector.read(&app, |view, ctx| {
            let listed = view
                .mixer
                .as_ref(ctx)
                .results()
                .iter()
                .map(|result| result.accept_result())
                .filter(|skill| skill.skill_name == name)
                .collect::<Vec<_>>();
            assert_eq!(listed.len(), 2);
            assert!(listed
                .iter()
                .any(|skill| skill.skill_reference == SkillReference::Path(second_path.clone())));
        });

        input.update(&mut app, |input, ctx| {
            input.handle_inline_skill_selector_event(
                &InlineSkillSelectorEvent::SelectedSkill {
                    skill_name: name.to_owned(),
                    skill_reference: SkillReference::Path(second_path.clone()),
                },
                ctx,
            );
        });
        input.read(&app, |input, ctx| {
            let detected = input
                .slash_command_model
                .as_ref(ctx)
                .detect_command(&format!("/{name} verify this channel"), ctx);
            let SlashCommandEntryState::SkillCommand(skill) = detected else {
                panic!("the selected skill should remain invocable");
            };
            assert_eq!(skill.reference, SkillReference::Path(second_path.clone()));
        });
        SkillManager::handle(&app).update(&mut app, |manager, ctx| {
            manager.handle_skills_added(vec![renamed]);
            ctx.emit(SkillManagerEvent::SkillsChanged);
        });
        input.read(&app, |input, ctx| {
            assert!(
                matches!(
                    input
                        .slash_command_model
                        .as_ref(ctx)
                        .detect_command(&format!("/{name} verify this channel"), ctx),
                    SlashCommandEntryState::None
                ),
                "an unavailable selected skill must not fall back to another same-named skill"
            );
        });
    });
}
