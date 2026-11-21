# Terminal and workspace

Galaxy combines normal terminal sessions with local project navigation, an editor, and agent conversations. Ordinary shell commands and agent-requested background commands use different execution paths; choose the normal terminal when you need interactive input.

## Find an action

Use the Command Palette or the application menus to find actions by name. The current shortcut is shown by the UI and can be customized in **Settings → Keybindings**. This is more reliable than assuming a shortcut from another terminal or from an upstream Warp guide.

For agent-related actions, type `/` in the input to see available commands. Some commands depend on the current session, enabled features, or provider configuration. Useful built-in entries include `/agent`, `/add-mcp`, `/background-jobs`, `/ai-activity`, and `/skills`.

An inherited command or menu entry can still be incomplete. In particular, `/docker-sandbox` and hosted environment setup are covered by the [known limitations](https://github.com/rkw6086/galaxy/wiki/Troubleshooting#docker-and-hosted-environments).

## Tabs, panes, and projects

Use the new-tab control or menu to open a normal shell session. Use split actions when you need two panes visible at once. Select the pane before typing a command or choosing a conversation, and check its current working directory before running a project-specific action.

Tab names and groups help organize work; they do not change a command's working directory or provider permissions by themselves. A restored pane can also be different from a newly started shell. Include the pane/tab/split sequence in a bug report if startup, focus, or closing behavior is unexpected.

The built-in file/editor views provide local project access. Opening a file or viewing a diff is different from approving an agent's proposed edit. The active profile still governs agent mutations and tool execution.

## Terminal blocks and agent context

Shell input and output appear as terminal blocks, with block actions for selecting or copying relevant content. Attach the specific context needed for an agent task rather than assuming the model can see every historical command or every file in a project.

Agent output can refer to an independent background job. Inspect `/background-jobs` for the actual command, status, logs, and exit code. A running background command does not become interactive merely because a terminal pane is visible.

## Appearance and shell behavior

Use **Settings → Appearance** for themes and visual preferences, and the terminal/features settings for shell and input behavior. Change one setting at a time when investigating rendering, prompt, selection, or notification issues.

Custom themes are loaded from the active Galaxy data directory's `themes` folder. On the normal macOS/Linux channel this is `~/.galaxy/themes`. A theme file controls appearance; it does not grant shell, provider, or filesystem permissions.

Shell startup files are owned by your shell, not by the color theme or tab group. If a command or PATH entry is available in one session and missing in another, compare the selected shell, launch mode, environment, and working directory. See [Troubleshooting](https://github.com/rkw6086/galaxy/wiki/Troubleshooting#background-command-problems).

## Launch configurations and workflows

Launch configurations save arrangements of windows, tabs, panes, working directories, and optional startup commands. Use the app's save/open launch-configuration actions to generate a valid file, then inspect it before reusing it. Startup commands execute when that configuration is opened; review downloaded or shared configurations before running them.

The normal macOS/Linux data locations include:

| Content | Directory |
| --- | --- |
| Launch configurations | `~/.galaxy/launch_configurations` |
| Tab configuration files | `~/.galaxy/tab_configs` |
| Custom themes | `~/.galaxy/themes` |
| User workflows | The workflows directory under the active Galaxy data directory |

Developer/data profiles and Windows can use other platform-specific paths. The application-generated file/help path is the authority for the active profile. Legacy field names in a generated configuration may remain for compatibility.

## Implementation references

- [Built-in slash command definitions](https://github.com/rkw6086/galaxy/blob/master/app/src/search/slash_command_menu/static_commands/commands.rs)
- [Workspace and tab actions](https://github.com/rkw6086/galaxy/blob/master/app/src/workspace/action.rs)
- [Keybinding settings](https://github.com/rkw6086/galaxy/blob/master/app/src/settings_view/keybindings.rs)
- [Configuration directories and generated launch comments](https://github.com/rkw6086/galaxy/blob/master/app/src/user_config/mod.rs)
- [Appearance settings](https://github.com/rkw6086/galaxy/blob/master/app/src/settings_view/appearance_page.rs)
