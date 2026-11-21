# Providers and models

Galaxy connects to the providers you configure. A connection supplies authentication, an endpoint or runtime, and a model catalog. A profile chooses models and controls the agent's actions. These are separate settings: configuring a provider does not authorize every tool or allow every orchestration model.

## Add a connection

Open **Settings → Providers**, choose the provider type, enter its connection details, and use the connection test/model-discovery step. Review the discovered models, enable the ones you want, then save the connection. A useful connection name identifies the destination, such as `Work LiteLLM` or `Local server`.

| Connection | What Galaxy needs | Execution ownership |
| --- | --- | --- |
| OpenAI | API key and API base URL; the wizard defaults to `https://api.openai.com/v1` | Galaxy's direct-provider runtime and tools |
| LiteLLM / compatible gateway | The gateway's OpenAI-compatible base URL and any required credential | Galaxy's direct-provider runtime and tools |
| ChatGPT subscription | The in-app ChatGPT authentication flow | Galaxy's native subscription transport and tools |
| Anthropic API | Anthropic API credentials | Galaxy's native Messages transport and tools |
| AWS Bedrock | Configured AWS authentication, region, and available model/profile inventory | Galaxy's native Bedrock runtime and tools |
| ACP agent runtime | A supported external coding-agent connection | The external agent owns its session, model, login, and tool loop |

The code also contains native Gemini and Vertex AI runtimes. Their presence in the runtime architecture does not imply that every build exposes a complete first-run connection wizard for them; the normal provider-choice list should be the guide to currently offered setup.

### OpenAI-compatible gateways and local servers

Use the base URL expected by your server, including its API prefix. A server exposing `/v1/models` commonly uses a base URL ending in `/v1`. Avoid appending `/chat/completions` as the connection base URL.

Use a unique connection name and keep a model attached to the correct connection. Galaxy routes models from `ai.providers` to their owning endpoint rather than choosing an endpoint only from the model's display name.

LiteLLM discovery can combine `/models` with `/model/info` metadata. Other compatible servers may expose only a model list; capabilities and token limits may require review. A successful connection test or discovery is not proof that every listed model can execute tools, accept images, or handle the same request profile.

### AWS Bedrock

Choose the intended AWS authentication method and profile, then check the region. The connection form supports the region from the AWS profile or an explicit override. Credentials must have access to the requested Bedrock models and, where used, inference profiles in that configuration.

Discovery reads the live AWS model and inference-profile inventory. The bundled model catalog enriches metadata; it is not an authorization allowlist. An inventory result, a cached model name, and authorization to invoke a model are different things.

Use the connection's explicit **Test** when you need a fresh discovery/access check. Model toggles and ordinary preference saves are not intended to rescan the entire AWS inventory. If access fails, check the actual AWS account/profile, region, model entitlement/agreement, and error returned for that invocation. Repeatedly toggling unrelated models is not a substitute for fixing access.

### Claude subscription and ACP

Galaxy labels its direct Claude subscription-token integration **unsupported** and displays an account/access warning. Do not confuse it with an Anthropic API key or with Claude Code's own supported sign-in flow.

For the separate **Claude Code ACP** connection, explicit setup/test may install pinned official Claude Code and ACP adapter packages into Galaxy's private per-user data directory. Normal agent launches do not download dependencies. Claude Code owns its browser sign-in, credential storage, and refresh; Galaxy does not read Claude Code's credential store.

For direct Anthropic API use, Galaxy owns the request and tool lifecycle. For ACP, Galaxy can display the messages it exchanges with the agent, but cannot expose that agent's internal model requests or compaction.

## Choose models in a profile

Select the active profile and review its base model and any configured terminal-use, coding, or computer-use model roles. Review its allowed orchestration providers as well. A role-specific model can use a different context budget from the base model.

An explicit provider selection restricts orchestration to those configured provider names. An explicitly empty selection permits no providers. Older/unrestricted profiles may include all configured providers. If an agent selects an unexpected destination, check the active profile and connection name first, not only the visible base-model picker.

See [Profiles, permissions, and tools](https://github.com/rkw6086/galaxy/wiki/Profiles-Permissions-and-Tools) for action authorization and MCP access.

## Understand context and reply limits

These values are not interchangeable:

- **Maximum context:** the provider's largest advertised window.
- **Default/efficient input budget:** the smaller positive limit Galaxy uses for ordinary requests, together with any profile cap.
- **Reply/output limit:** the space reserved for the model's next response.
- **Fixed request cost:** system prompts, tools, and other request overhead.
- **Conversation history:** the remaining content that can fit after those reservations.

Galaxy preserves a lower positive default/input/context limit when merging discovery metadata. A larger advertised maximum does not automatically opt the session into an extended context variant. Profile limits and the configured effective-context percentage also affect the usable window.

Compaction can begin before a display reaches the provider's advertised maximum. It reserves room for output and may prepare history for a smaller terminal-use profile. This is different from losing messages without accounting: inspect the activity capture when investigating a specific turn.

If a gateway advertises an output limit as large as its entire context window, report the exact discovered limits and provider/model identity. A maximum context value should not be copied into an output setting merely because both fields contain token counts.

## Advanced settings

Prefer the setup UI for credentials and model discovery. Galaxy also retains settings for legacy single-endpoint configuration (`ai.openai.*`) and multiple named connections (`ai.providers`). Treat the settings file as sensitive: provider entries may contain credentials.

A configuration shape without credentials looks like:

```toml
[ai.openai]
enabled = true

[[ai.providers]]
name = "My compatible gateway"
kind = "litellm"
base_url = "https://gateway.example/v1"

[[ai.providers.models]]
model_id = "your-server-model-id"
display_name = "My model"
context_size = 128000
default_context_size = 128000
```

The endpoint, model ID, and limits are placeholders. Use values supplied by your server, and enter any required credential using the provider UI. Do not paste this example over an existing configuration without preserving its other connections.

## Implementation references

- [Provider setup choices and connection workflow](https://github.com/rkw6086/galaxy/blob/master/app/src/settings_view/provider_setup_view.rs)
- [Provider settings and model configuration types](https://github.com/rkw6086/galaxy/blob/master/app/src/settings/ai.rs)
- [Model preferences and routing](https://github.com/rkw6086/galaxy/blob/master/app/src/ai/llms.rs)
- [Bedrock discovery](https://github.com/rkw6086/galaxy/blob/master/app/src/ai/provider/discovery.rs)
- [Direct Claude subscription warning and credential references](https://github.com/rkw6086/galaxy/blob/master/app/src/ai/claude_subscription.rs)
- [ACP setup](https://github.com/rkw6086/galaxy/blob/master/app/src/ai/acp/setup.rs)
