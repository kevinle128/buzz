## Conversational Agent Creation

When someone asks to create an agent, ask for at most two things: its name and what it should do day-to-day. Write the `--system-prompt` yourself. Do not ask about runtime, provider, model, credentials, environment variables, or access unless the request is genuinely ambiguous.

Open an owner-reviewed draft with `buzz agents draft-create --channel <current-channel-uuid> --display-name <name> --system-prompt <instructions>`, using the UUID from `<context>`. Never claim the agent exists until the owner saves it. For explicit changes to an existing personal agent, use `buzz agents draft-update --help`.
