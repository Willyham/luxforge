# MCP adapter

Status: implemented ([task plan](../../tasks/programmability/mcp.json), [feature status](../features.md), [user guide](../user-guide.md#connecting-an-agent)). Verified on Linux by the unit and process tests, the `rmcp` interoperability test and the `settings-agents` and `mcp` scenarios under Xvfb. Open: the M4 runs, the macOS release rehearsal, Windows, and qualification with real clients ([verification](#verification)).

`luxforge-mcp` is a Model Context Protocol server. It lets an agent (Claude Code, Claude Desktop, Codex, Cursor and other MCP clients) operate Luxforge through the same operation registry that the desktop, `luxforge-json` and `luxforge-ctl` use. It is an adapter and has no feature logic of its own. Every tool it offers is a registry method, every edit is an ordinary API transaction with the same history, undo and conflict behaviour as a person's, and every argument schema is generated from `schema.list`. There is no hand-written copy.

## Goal

A person adds Luxforge to their agent once, from **Settings › Agents** or the user guide. From then on, while Luxforge is open, the agent can:

- read the catalog and the open photograph;
- make edits that land in history under the agent's name;
- wait for jobs;
- see the result as a picture.

All of this uses the operations the person has. An agent cannot do anything the live session cannot do. In particular, it can never grant consent or permission.

## Shape

- **One binary.** `luxforge-mcp` is a third binary in the `luxforge-cli` package. It reuses `luxforge_cli::live` (session discovery, connection, handshake and envelope construction) and keeps the package's headless rule: no Iced, wgpu or desktop crates.
- **Transport.** The only transport is stdio, JSON-RPC 2.0, one message per line. The MCP client starts the process and owns its lifetime.
  - Diagnostics go to stderr only.
  - There is no Streamable HTTP listener, because a local network endpoint would need its own origin checks and authentication, and stdio is what local clients launch.
  - The official conformance suite tests servers over HTTP and runs on Node. The project keeps all tooling in Rust, so [verification](#verification) uses Rust tests against the vendored specification schemas and an independent Rust client instead.
- **Implementation.** The protocol layer is hand-written: synchronous, over `serde_json`, with a reader thread and one worker, in the style of `luxforge-ctl`. The surface it needs is small: discovery, the legacy handshake, `tools/list`, `tools/call`, cancellation and list-change subscriptions. `rmcp`, the official Rust SDK, was considered as the server. It releases roughly weekly at 3.x and brings an async server stack this adapter does not need, so it is used only as an independent test client ([verification](#verification)).

## Protocol versions

The adapter is **dual-era**, as the [2026-07-28 versioning rules](https://modelcontextprotocol.io/specification/2026-07-28/basic/lifecycle) define it. That revision is the current one: it is stateless and has no `initialize` handshake. Many clients still speak 2025-11-25.

- **Modern (2026-07-28).**
  - Each request carries `io.modelcontextprotocol/protocolVersion`, `clientCapabilities` and, optionally, `clientInfo` in `_meta`. Each result carries `io.modelcontextprotocol/serverInfo` and `resultType: "complete"`.
  - `server/discover` advertises the supported versions, the `tools` capability (`listChanged` in the typed profile) and the server's identity.
  - An unsupported version gets `UnsupportedProtocolVersionError` (`-32022`), listing the supported versions.
  - `tools/list` returns `ttlMs` and `cacheScope: "private"` and lists tools in a deterministic order.
  - `subscriptions/listen` serves `toolsListChanged`.
  - `notifications/message` is never sent, because Logging is deprecated.
- **Legacy (2025-11-25 and 2025-06-18).**
  - An `initialize` request selects legacy semantics for the life of the process. The server negotiates the client's version when it supports it, and otherwise answers with 2025-11-25.
  - It then serves `tools/list`, `tools/call`, `ping` and `notifications/tools/list_changed` as those revisions define them.
  - Results never carry `resultType`, and `_meta` is not required.
- **Both eras.**
  - `notifications/cancelled` for a queued request drops it. For a running call, it stops the adapter's own waiting at its next hold. It never cancels a Luxforge job: that is `job.cancel`, an operation of its own.
  - There are no resources, prompts, sampling, roots, elicitation or tasks. Luxforge jobs stay jobs, read with `job.read` and `job.wait`. The `io.modelcontextprotocol/tasks` extension is later work.
- **One call at a time.** Requests are served one at a time, in arrival order, on one live connection, which keeps session-scoped state coherent. At most 32 requests are queued; beyond that a request is refused with a JSON-RPC error that names the limit.
  - An incoming line is bounded at 1 MiB, as the owner's request limit is.
  - A held wait, such as `job.wait` or `events.wait`, is at most 30 s, so a queued call waits at most that long behind it.

## Attaching to Luxforge

- **The running desktop.** By default the adapter attaches to the running desktop's live session, through the same registry of running sessions and `--catalog PATH` rule as `luxforge-ctl` ([live CLI](live-cli.md#targeting-and-safety)). It is an ordinary live client with Edit authority.
- **Attaching late.** Agents often start their MCP servers before Luxforge is open, so the adapter starts, answers discovery and lists its tools without a session.
  - It attaches at the first tool call that needs the owner. With no session, that call returns a tool error, `no-session`, saying to open Luxforge. With several sessions it returns `usage`, listing each catalog, since the adapter never guesses.
  - When the connection is lost (the desktop quit or restarted), the in-flight call reports `connection-lost`. A mutation reports `outcome-unknown`, with its method and `request_id`, and is never retried.
  - The next call discovers the session again. The first result after a reconnect carries a `session-replaced` warning: drafts, previews and `clients` jobs belonged to the old connection and are gone.
- **Identity.** On connecting, the adapter names itself with `session.identify` ([below](#core-additions)), using the MCP client's `clientInfo` title or name followed by "via MCP", for example "Claude Code via MCP". Its mutations use the actor `mcp:<client name>`, bounded to the envelope's 128 characters, or `mcp` when the client gives no name, so a history row shows which agent made the edit.
- **Headless.** `--headless --catalog PATH` starts the adapter's own `luxforge-json --catalog PATH` child, with Edit authority and never `--permission-authority`, and speaks to it over the child's stdio. `--headless` without `--catalog` is a usage error.
  - The child starts lazily, at the first tool call that needs the owner, as a live attachment does, so discovery and the tool list stay instant and a client that never calls a tool starts nothing.
  - It refuses with a named error, `catalog-in-use`, when a running desktop's registry entry names that catalog, before any child starts. The catalog's exclusive SQLite lock refuses a second owner anyway; the registry check makes the message clear.
  - A child that cannot be started, or ends or does not answer during its handshake (the catalog's lock among the causes), is reported as `headless-failed` with what it wrote to stderr. It is not retried: the next call that needs the owner makes one new attempt of its own.
  - A child that ends while serving is a lost connection, under the same rules as a desktop that quit: the call in flight reports `connection-lost`, or `outcome-unknown` for a mutation, which is never sent again. The next call starts a new child and carries the `session-replaced` warning.
  - The child lives exactly as long as the adapter. On stdin end-of-file the adapter closes the child's stdin, waits a bounded time, then kills it. The child's stderr is forwarded to the adapter's stderr.
  - A headless owner draws nothing, so it renders on the reference renderer and `session.state` reports that. A method a headless owner cannot serve fails explicitly as it already does.

## Tools

One generator turns the live `schema.list` into tools. Which tools are offered is chosen with a launch flag, `--tools`:

| `--tools` | Tools offered |
| --- | --- |
| `compact` (default) | Three generic tools, a fixed list whether or not a session exists |
| `all` | One typed tool per registry method |
| `NS[,NS…]` | One typed tool per method in the named namespaces, e.g. `asset,edit,query,render,history,job,preview` |

The default is compact, because the full registry is about 240 methods. Their notes and parameters are nearly 290,000 characters, roughly 80,000 tokens of tool definitions. Typed tools are opt-in, for clients that defer large tool lists or for a person who wants to approve operations one by one.

### The compact profile

- **`luxforge_methods {namespace?, query?}`** lists methods, in registry order. Each entry has its name, a one-line summary (the first sentence of its notes), `mutates`, `session_scoped`, its effect markers and whether a live client can call it at all. The query filter matches names and notes, case-insensitively. Read-only.
- **`luxforge_describe {methods: [name…]}`** returns, for each named method (up to 16), its generated input schema, its full notes and its envelope. Read-only.
- **`luxforge_call {method, params?, expected_revision?}`** calls any method. Before sending, it validates `params` against that method's generated schema and refuses a method a live client cannot call. It builds the envelope as described under [envelopes](#envelopes) and returns the result as described under [results](#results).
  - Its annotations are the conservative defaults (`readOnlyHint: false`, `destructiveHint: true`, `openWorldHint: true`), because it can call anything.

Without a session, `luxforge_methods` and `luxforge_describe` answer from the built-in registry of the adapter's own build and say so in a `source: "built-in"` field. The live registry can differ, for example under developer mode or an experiment.

### The typed profiles

- **One tool per method.** A tool's name is the method's name with `.` replaced by `_`, because some model APIs refuse dots in tool names; `asset.state` becomes `asset_state`. A test holds the mapping collision-free and every name within 64 characters. The tool's `title` is the method's own name.
- **Description.** The description is the method's full notes. A session-scoped method's adds that its state lives on the adapter's own connection, as long as the process stays attached.
- **Namespaces.** A namespace names the methods whose name is it or starts with it and a dot, so `module.settings` names one as `edit` does. A namespace is checked at launch against the registry built into the adapter, since there may be no session yet: one that names none of its methods is a usage error that lists the namespaces there are.
- **Annotations.** These come from the registry's markers ([core additions](#core-additions)):
  - `readOnlyHint` is the method's `read_only`.
  - `destructiveHint` is true only for a method that changes something and cannot be taken back: `!read_only && !reversible`.
  - `idempotentHint` is false.
  - `openWorldHint` is true only for a method marked as reaching the network.
- **Methods left out.** A method that only permission authority can call is not offered, because a live client's call would always be `forbidden`.
- **Without a session.** The list comes from the built-in registry. Once attached, if the live registry differs, the adapter announces a list change.
- **List changes.** The registry changes when a flag or developer mode adds or removes methods. Once the agent has subscribed (modern) or after `initialize` (legacy), the adapter watches the owner's event log on a second live connection, with a held `events.wait`. After an event from `flags.set` (the registry has no module-enablement method today), it reads `schema.list` again and sends `notifications/tools/list_changed` if the generated list differs.
  - The second connection names itself after the agent with "(tool list)", for example "Claude Code via MCP (tool list)", so the desktop's list of clients never shows it as a second agent.
  - A headless child serves one connection, and in headless mode the adapter is the only client. So nothing watches: it reads the schema again only after its own calls to those methods, and announces a difference the same way.

### Schema generation

The generator turns each method's `parameters` (the [parameter vocabulary](architecture.md#agent-contract)) into a JSON Schema 2020-12 object schema:

- **Properties.** Each parameter becomes a property, with `required` taken from the descriptor and `additionalProperties: false`.
- **Descriptions.** A property's description holds its notes, unit, step, soft range and precision.
- **Mapping by kind:**
  - `integer` and `number` become `integer` and `number`, with `minimum` and `maximum`.
  - `enum` becomes `enum`.
  - `boolean` becomes `boolean`.
  - `string` becomes `string` with `maxLength`.
  - `text` becomes `string`, with its byte bound stated in the description and checked by the adapter, because `maxLength` counts characters, not bytes.
  - `identity` becomes the identifier's wire type, described as "a *kind* id".
  - `points` and `curve` become arrays of pairs with `minItems` and `maxItems`.
  - `color`, `artifact`, `settings` and `settings-origin` follow their wire forms.
  - `json` becomes `{}` (any value), with its notes as the description. The 75 `json` parameters give their shape only in prose today; publishing those shapes is [later work](#later).
  - `default` is copied.
- **Host fields.** A field an entry lists beside its declared parameters is typed as the host takes it: `asset_id` an asset id, `entry_id` an entry id, `profile_id` a string of at most 256 characters, and a maskable module's mask by the entry's own `target`. A field nothing types refuses the entry.
- **`null`.** An optional field of a host method also accepts `null`, which the host reads as absent or, for a field that can be cleared, as clearing it (`preferences.set {theme: null}`). An action's, query's or task's fields do not, because their check refuses it.
- **Envelopes.** The envelope never appears as a property ([envelopes](#envelopes)).
- **Unknown kinds.** A parameter kind the generator does not know fails a test, so a new kind cannot ship without its mapping. Live, it refuses the whole `schema.list` answer by name rather than offering a method with a guessed schema.
- **The request check.** `luxforge_cli::mcp::schema` also checks a call's arguments before anything is sent, with the core's own value check, so it refuses in the owner's words and names the parameter. It covers what a schema cannot say: a `text` bound in bytes, a `string`'s control characters, an identity's syntax, a curve's order and a settings set's action names. A generated schema never refuses what the check accepts.

### Envelopes

The adapter builds every mutation envelope from the method's `schema.list` entry, exactly as `luxforge-ctl` does:

- **Request IDs and actor.** The adapter generates `request_id` (UUID v4) and supplies `actor`. An agent cannot pass `mutation`.
- **`revision_of: asset`.** `expected_revision` is an optional extra property. When the agent passes it, it is used as given, so an agent that read state, reasoned and then edited gets an honest `conflict` if the person edited in between. When omitted, the adapter reads `asset.state` on the same connection immediately before the edit, as `luxforge-ctl --asset` does.
- **`revision_of: draft` and `module-settings`.** `expected_revision` is a required property.
- **Conflicts.** A conflict is returned as a tool error. The adapter never retries, rebases or refreshes an edit.

Session-scoped methods (`draft.*`, `preview.select`, `view.set` and the rest) are offered in every profile. The adapter keeps one connection for its whole life, so a draft begun in one tool call is there in the next. Drafts are explicit handles (`draft_id`), which is how the stateless revision wants cross-call state carried. Their lifetime, the adapter's process and its connection, is stated in those tools' descriptions.

### Results

- **Success.** A successful call returns the method's result as `structuredContent` and the same JSON as one text content block. There is no `outputSchema`, because the registry publishes result shapes only in notes.
- **Failure.** A Luxforge failure is a tool execution error (`isError: true`). Its text and `structuredContent` are `{error: {code, message, data?, job_id?}}`, preserved exactly, so a model can correct itself from `validation`, `conflict`, `preparation-required` or `consent-required`.
- **Protocol errors.** An unknown tool is JSON-RPC `-32602`. Arguments that fail the generated schema are a tool execution error naming the parameter.
- **Jobs.** A result that names a job is returned as the registry returns it. The agent follows it with `job.wait`, whose hold is at most 30 s. A job whose `ownership` is `clients` lives as long as the adapter's connection, and the result says so with a `job-released` style warning as `luxforge-ctl` does.
- **Pictures.** A result field that the registry marks as an image ([core additions](#core-additions)) is read by the adapter and attached as MCP image content (`image/jpeg`, base64), beside the structured result.
  - This covers `preview.read`'s path and fallback, and the path of a preview job's result (`preview.region`'s among them, which answers only a job) in a `job.read` or `job.wait` result.
  - The largest photo tier is 2048 px, so a picture is at most a few hundred kilobytes. The adapter refuses to attach a file over 8 MiB, and the result carries a warning instead.
  - Full-resolution exports are never attached. Their paths are returned as data.
  - `--no-images` turns attachment off for a launch.
  - The user guide says plainly that an agent's model provider may be remote, so an attached picture leaves the machine with the agent's other context.
- **Size.** Results are bounded by the live client's 64 MiB response limit and are never truncated. A list too large for an agent's context is the agent's to page, using the method's own paging parameters.

## Core additions

The registry gains the markers the adapter needs, so the adapter holds no per-method knowledge.

- **`authority: "permissions"`** on each method that only permission authority may call: `module.permission.grant`, `usage.prompt.claim`, `update.install`, `update.move-to-applications`, `catalog.empty-removed` and `session.disconnect`. A method that needs permission authority for one argument value, such as `usage.set {enabled: true}`, keeps that in its notes and stays offered.
- **Effect markers** on every method. `mutates` says only that a method carries a mutation envelope. Many methods change something without one (`preferences.set`, `view.set`, `draft.*`, `job.cancel` and the rest), so whether a method changes anything is `read_only`:
  - `read_only: true | false` on every method, true only when it changes nothing at all: no catalog, library, session, preference, flag, job or file state. Holding a wait is a read, and so is a call whose only effect is a derived, disposable record such as a preview, an analysis report or the folder index ([the rule](architecture.md#agent-contract)).
  - `reversible: true | false` on every method whose `read_only` is false, and on no read. A reversible change can be undone through history undo or library undo, set back by the same method with the earlier value, removed with what the call created, or is the caller's own session state, set again.
  - `network: true` for a method that can reach the network, never a read.
  - Every host method declares its classification explicitly, and a test refuses one that declares none, so a new operation cannot inherit a default.
- **`images: [field…]`** names result fields that hold the path of a JPEG the owner wrote for display. It is set on `preview.read` and `preview.region`, and declared per job kind for those fields in a job's result.
- **Client identity and connected clients:**
  - `session.identify {label}` is session-scoped and names the calling client, 1–64 characters, for the desktop's list. `luxforge-ctl` identifies itself as "luxforge-ctl".
  - `session.clients` lists the owner's other registered clients: `{client_id, label?, authority, transport, connected_at, last_request_at?, requests}`. It reports metadata only, never the content of requests.
  - `session.disconnect {client_id}` closes that client's connection. Its session ends exactly as a disconnect ends it today: drafts end and `clients` jobs it alone wanted stop. Only permission authority may call it, which means the desktop. A connection or disconnection is recorded in the event log, so an open Settings tab follows it without polling.
  - The events are `session.connect`, `session.identify` and `session.disconnect`, each naming `client_id`; the [architecture's connected clients](architecture.md#agent-contract) give the transports, refusals and how a JSON-lines stream is closed.

## Settings › Agents

A tab in the Settings sheet, after Models and before Experiments, with a terminal icon, also opened from the palette (Settings · Agents). Every build shows it. Its boards, at 1440 × 900 logical points like the other Settings boards, are [connected](mcp/connected.webp), [empty](mcp/empty.webp) and [missing binary](mcp/missing.webp), drawn from the standalone pages in [mcp/html](mcp/html).

- **Introduction.** One opening sentence: agents use the same operations you do, every edit is in history and undoable, and an agent can never grant consent or permissions.
- **Connect an agent.**
  - The tab shows the `luxforge-mcp` beside this build: `Contents/Helpers/luxforge-mcp` in the macOS bundle, or beside the editor elsewhere. A development build without one says it is missing and names the build command; nothing is drawn as if it worked.
  - A segmented choice of Claude Code, Claude Desktop, Codex, Cursor and Other (MCP JSON) shows that client's configuration with Copy, the file it goes in and one line saying what to do ([client configuration](#client-configuration)). For the bundled server:
    - Claude Code: `claude mcp add --scope user luxforge -- '/Applications/Luxforge.app/Contents/Helpers/luxforge-mcp'`, run in a terminal. The user scope makes Luxforge available in every project, not only the folder the command runs in.
    - Claude Desktop: the `mcpServers` block for `claude_desktop_config.json`, which Claude Desktop opens from Settings › Developer › Edit Config, then quit and reopen it.
    - Codex: the `[mcp_servers.luxforge]` table, with `command` and `args`, for `~/.codex/config.toml`.
    - Cursor: the `mcpServers` block, with `"type": "stdio"`, for `~/.cursor/mcp.json` or a project's `.cursor/mcp.json`.
    - Other: the same `mcpServers` block, with `command` and `args`, that most clients read.
  - Luxforge copies text to the system clipboard. It never edits another application's configuration.
  - The snippets come from one core function, also printed by `luxforge-mcp config CLIENT`, so the tab and the command line agree and the text is testable without the interface.
- **Connected.**
  - Every other live client is listed: its label (for example "Claude Code via MCP", "luxforge-ctl", or "Unnamed client"), how long it has been connected, its last activity and its request count.
  - Each row has **Disconnect** (`session.disconnect`).
  - With none connected, one line says so.
  - The list follows connections live while the tab is open.
- **What it reads.**
  - Opening the tab reads the server beside this build (one file check) and `session.clients` in one task off the update loop. Disconnect sends `session.disconnect` through the desktop's own client, which holds permission authority, and the list is read again when it answers; a refusal is said under the list and in the status bar, and the row's button is disabled while its request is out.
  - There is no polling. A transport client's connection, identification and disconnection are recorded in the event log, which wakes the desktop's event sync; a `session.*` event, or a gap, reads the list again while the tab is open, one read at a time, and nothing while it is closed. Connected and last-activity times are said to the minute when the tab is drawn.
  - The configuration is the core's snippet for the server found, without a catalog or `--headless`; with none found there is no chooser or Copy. Copy writes the shown text to the system clipboard and says "Copied the *client* configuration" in the status bar.
- **Evidence.** The step `{"agents": …}` drives the tab's own messages: `{"listed": [labels]}` captures once the list shows exactly those labels, `{"client": id}` chooses a client, `"copy"` presses Copy and `{"disconnect": label}` presses that row's Disconnect, capturing once the list no longer shows it. A frame's `settings.agents` records the server as looked for, the chosen client, the configuration and rows as drawn, what Copy copied and `session.clients` as the desktop last read it. The `settings-agents` smoke scenario captures the empty list, Codex's configuration copied and Claude Code's, a real `luxforge-mcp` started by the harness and listed as "Claude Code via MCP", and its Disconnect; a second launch runs the editor alone in a folder of its own and captures the missing state.

There is no switch to turn live sessions off. That is [later work](#later).

## Statistics and privacy

The adapter adds no statistics. A call through a live session on an opted-in desktop is counted at the shared operation boundary, as a `luxforge-ctl` call is ([usage statistics](usage-statistics.md)), and usage events carry no client label. A headless owner never collects. Image attachment sends pixels only to the MCP client the person configured, never anywhere else.

## Performance and bounds

The adapter is a separate process that holds at most two live connections: one serving calls, and the event watch in a typed profile. The owner allows eight clients, and a ninth gets the owner's `resource-limit`.

| Bound | Value |
| --- | --- |
| Incoming line | 1 MiB |
| Queued requests | 32 |
| Calls in flight | 1 |
| Response | 64 MiB |
| Attached image | 8 MiB |
| Held wait | 30 s |

The adapter has no polling loop: waits are the owner's held `events.wait` and `job.wait`. The core additions are metadata on the static method table plus three small session methods; `session.clients` reads the owner's existing client table. Nothing renders, decodes or blocks on the catalog owner's thread.

## Implementation

Everything above is built: the core additions, the live-session adapter and headless mode with the compact and typed profiles and list-change notifications, `config` with its client configuration, packaging beside `luxforge-ctl`, Settings › Agents with its `settings-agents` scenario, and the `rmcp` interoperability test.

### Command line

`luxforge-mcp [--catalog PATH] [--headless] [--tools compact|all|NS[,NS...]] [--no-images]`, with `--help` and `--version`, which answer without serving. `luxforge-mcp config CLIENT [--headless] [--catalog PATH]` prints one client's configuration for its own executable instead ([client configuration](#client-configuration)). `--tools` takes `compact`, `all` or a comma-separated list of namespaces ([the typed profiles](#the-typed-profiles)); a namespace the built-in registry has no method in, an empty one, `--headless` without `--catalog`, a flag given twice and any other argument are usage errors: `{"error": {code: "usage", message}}` on standard error and exit status 2. The server ends with exit status 0 when standard input ends: requests still queued are dropped, the one being answered finishes first, and a headless child has ended. Diagnostics (attaching, a closed or lost connection, an unreadable registry, a headless child's start and end, and the child's own standard error) go to standard error as plain lines, never the session token.

### Client configuration

`luxforge_core::agents` makes every client's configuration from the server's absolute path and its optional arguments; the desktop's Settings › Agents tab and `luxforge-mcp config` both call it, so they print the same text.

- **Input.** A `Launch`: the absolute path of `luxforge-mcp`, and optionally `--catalog PATH` (absolute), with `--headless` before it to start its own owner. A relative or non-Unicode path, and `--headless` without a catalog, are refused: a client starts the server from a folder of its own choosing, and no configuration format holds a non-Unicode path.
- **Output.** A `Snippet` per client: the client, the kind of text (a command, JSON or TOML), the text, the file it goes in on that platform and one instruction line. The server is always named `luxforge`.
- **Clients.**

  | Client (`config` id) | Text | Where it goes |
  | --- | --- | --- |
  | Claude Code (`claude-code`) | `claude mcp add --scope user luxforge -- PATH [ARGS]`: each path in single quotes, a quote inside escaped as POSIX shells read it on macOS and Linux, and doubled for PowerShell on Windows | A terminal |
  | Claude Desktop (`claude-desktop`) | `{"mcpServers": {"luxforge": {"command", "args"}}}` | `~/Library/Application Support/Claude/claude_desktop_config.json` on macOS, `%APPDATA%\Claude\claude_desktop_config.json` on Windows and `~/.config/Claude/claude_desktop_config.json` on Linux |
  | Codex (`codex`) | `[mcp_servers.luxforge]` with `command` and `args` as TOML basic strings | `~/.codex/config.toml` (`%USERPROFILE%\.codex\config.toml` on Windows) |
  | Cursor (`cursor`) | `{"mcpServers": {"luxforge": {"type": "stdio", "command", "args"}}}` | `~/.cursor/mcp.json` (`%USERPROFILE%\.cursor\mcp.json` on Windows), or a project's `.cursor/mcp.json` |
  | Other (`json`) | `{"mcpServers": {"luxforge": {"command", "args"}}}` | The client's own configuration |

  Anthropic documents Claude Desktop's configuration file for macOS and Windows; its Linux release is a beta, and the Linux path is the one its configuration folder takes there. The instruction names Settings › Developer › Edit Config, which opens the file on every platform.
- **Finding the server.** `agents::server_beside` is where it ships relative to the editor's own canonical executable: `Contents/Helpers/luxforge-mcp` in a macOS bundle, beside the editor otherwise (the Linux payload and a development build's target folder). Whether it exists is the caller's to check.
- **`config`.** `luxforge-mcp config CLIENT` passes its own canonical executable path (`current_exe`), and a `--catalog` made absolute from the current folder. It prints the snippet's text on standard output and where it goes and what to do on standard error, and writes no file. An unknown or missing client is a usage error listing the five ids, exit status 2.
- Nothing reads or writes another application's configuration.

### Packaging

`luxforge-mcp` is the fourth release executable (`EXECUTABLES` in `xtask/cli/src/release_candidate.rs`): `Contents/Helpers/luxforge-mcp` in the macOS bundle, signed with the other helpers before the app, and `Luxforge/luxforge-mcp` in the Linux payload, mode 0755 in both archives. The release build checks its `--version` against the tag, its hashes are bound in the build records before and after signing, the Linux record lists its ELF imports, and the Linux `INSTALL.txt` names it and its `config` command.

### Module layout

The library side is `luxforge_cli::mcp`; the binary is `src/luxforge_mcp.rs`.

- `protocol` reads one line as a JSON-RPC message and decides each request's era: modern when its `_meta` names `io.modelcontextprotocol/protocolVersion`, legacy after `initialize`. It builds results (a modern one gains `resultType: "complete"` and `_meta["io.modelcontextprotocol/serverInfo"]`), errors and notifications, the label and the actor.
- `serve` is the reader thread and the one worker over a bounded queue. The reader frames standard input, refuses an unreadable line, an over-long line or a full queue at once, and applies `notifications/cancelled` at once. The worker answers queued requests in arrival order through a `Handle`. Both write whole lines through one shared output.
- `adapter` is the `Handle`: discovery, `initialize`, `ping`, `subscriptions/listen`, `tools/list` and `tools/call`.
- `tools` holds the `Profile` (`Compact`, or `Typed` with its `Namespaces`), the three compact tools and the typed generator. `luxforge_call`'s path (check, envelope, one send, job warning, pictures) is one function each typed tool's call goes through too.
- The built-in registry is not assembled at run time. `src/mcp/builtin-schema.json`, built into the binary, is `schema.list` as `ModuleRegistry::builtin()` answers it, parsed once when first needed: by a listing without a session and by `--tools` checking its namespaces at launch. The adapter therefore links no module and builds no registry. A unit test (`the_built_in_schema_matches_the_registry`) regenerates the answer from the registry and fails when it differs from the file; after an intended registry change, rewrite it with `cargo test -p luxforge-cli --lib -- --ignored generate_builtin_schema_snapshot` and review the diff.
- `changes` keeps a typed profile's list current and announces its changes ([typed tools and list changes](#typed-tools-and-list-changes)).
- `owner` is the seam to a catalog owner: a `Target` attaches (`LiveTarget` finds the live session and connects with `LiveClient::connect_as`) and a `Connection` carries calls (`LiveClient` implements it). `Owner` attaches lazily, ends a session whose connection failed, and warns `session-replaced` on the next attachment. It reports each attachment's registry to `changes`, and adopts a newer one the event watch read before its next call.
  - `Target::list_changes` says how registry changes reach the adapter. `Watch`, the default and the live target's, leaves them to the event watch, whose connection `Target::again` reaches: for the live target, the session its last attachment found, never another found since. `OwnCalls`, the headless target's, says the adapter is the owner's only client on its only connection, so nothing watches: `Owner::call` reads `schema.list` again after each of its own successful calls to a method in `REGISTRY_CHANGES` (`flags.set`; the registry has no module-enablement method), replaces the session's registry and generated inputs when it differs, and reports it to `changes`. Through `luxforge-json`, whose `schema.list` never depends on flags, the answer is the same today.
- `headless` is the other `Target` and `Connection` pair ([headless](#headless)).
- `images` finds pictures through the `images` and `job_images` markers and reads them within the bound.
- `schema` generates the input schemas and the request check ([schema generation](#schema-generation)).

### Protocol details

- **Versions.** `server/discover` and a refusal list `["2026-07-28", "2025-11-25", "2025-06-18"]`. A request whose `_meta` names any other version, a legacy one included, is refused with `-32022` and `data: {supported, requested}`. A modern request missing `clientCapabilities`, and any request naming no version before `initialize`, is `-32602`. `initialize` answers the client's version when it is 2025-11-25 or 2025-06-18, and 2025-11-25 otherwise; a second `initialize` is `-32600`. A modern request is still served as one after `initialize`.
- **Methods.** Modern: `server/discover`, `subscriptions/listen`, `tools/list`, `tools/call`. Legacy: `initialize`, `ping`, `tools/list`, `tools/call`. Anything else is `-32601`. A `tools/list` cursor is `-32602`: the list has one page.
- **Caching.** `server/discover` answers `ttlMs: 3600000` and `cacheScope: "private"`. Modern `tools/list` answers the same in the compact profile, whose list is fixed for the life of the process, and `ttlMs: 60000` in a typed one, whose list can change.
- **Capabilities.** `{tools: {}}` in the compact profile, whose list never changes, and `{tools: {listChanged: true}}` in a typed one. `subscriptions/listen` is acknowledged with `notifications/subscriptions/acknowledged` and stays open, unanswered, until the client cancels it. The compact profile honours nothing (`notifications: {}`); a typed one honours `toolsListChanged` when it is asked for.
- **Instructions.** Discovery and `initialize` carry a short `instructions` text. The compact profile's names the three tools; a typed profile's says how a tool's name comes from its method, that `expected_revision` goes beside the method's parameters, and that the list follows Luxforge's registry. Both say the tools operate the catalog the running desktop has open or the one the server was started headless over, and that edits land in history under the agent's name.
- **Framing errors.** An unparsable line is `-32700` and a message that is not a JSON-RPC request object (a batch, a null or fractional `id`, non-object `params`) is `-32600`, each without an `id` unless one could be read. A line over 1 MiB is `-32600` with `data: {limit}`, unread. A request arriving while 32 wait is refused at once with the application code `-31000` and `data: {limit: 32}`, outside the JSON-RPC reserved range as the 2026-07-28 code policy asks.
- **Cancellation.** A cancelled queued request is removed and never answered. A cancelled running call stops at its next hold (after attaching, after the `asset.state` read, before the job read) and its answer is withheld; a call already sent to the owner, such as a held `job.wait`, finishes there and its answer is dropped.
- **Legacy results.** `structuredContent` is included only when the result is a JSON object, as 2025-11-25 requires; the text block always carries the JSON.

### The compact tools

- **`luxforge_methods`.** `{source, catalog | session, count, methods}`. `source` is `live` with the session's `catalog`, or `built-in` with `session`, the failure that left no session (`no-session`, or `usage` listing several). It tries to attach, so it reads the running editor's registry when there is one. Each entry is `{name, summary, mutates, session_scoped, read_only?, reversible?, network?, callable}`: the markers as the entry gives them, and `callable` false for `authority: "permissions"`. The summary is the notes up to the first `. ` or `; `, at most 240 characters. `namespace` keeps a name equal to it or starting with it and a dot, so `module.settings` works as well as `edit`. Names are listed sorted, as `schema.list` gives them.
- **`luxforge_describe`.** `{source, catalog | session, methods}`, each method once: `{name, notes, mutates, session_scoped, read_only?, reversible?, network?, callable, envelope, revision_of?, expected_revision?, session?, input_schema}`. `envelope` is `revision`, `request` or null; `expected_revision` is `optional` or `required` when the method has one. `input_schema` is the generated schema of `params`, without `expected_revision`, which `luxforge_call` takes beside `params`. `session` states a session-scoped method's lifetime. An unknown name, or more than 16, is a `validation` tool error.
- **`luxforge_call`.** `expected_revision` is given beside `params`, never inside them. A `validation` tool error names the argument in `data.parameter`: `method`, `params`, `expected_revision` or `params.<name>`. A method with `authority: "permissions"` is refused `forbidden` before anything is sent. The registry's own failures pass through unchanged.

### Typed tools and list changes

- **Generation.** `tools::typed` builds the list from a registry and its generated inputs, in the registry's sorted order: one tool per callable method the profile's namespaces offer, `{name, title, description, inputSchema, annotations}`. A name over 64 characters or two methods with one name fail the generation, so the list is never offered with a tool missing. `inputSchema` is the method's generated schema with `expected_revision` beside its parameters when the method takes one.
- **Calls.** A typed tool's arguments are the method's parameters, with `expected_revision` taken out and sent in the envelope. The tool is looked up in the registry the call would use; a name it does not offer is `-32602`. Without a session, a call is a tool error carrying the failure that left none. Otherwise the call is `luxforge_call`'s, and a `validation` error names a parameter by its own name rather than `params.<name>`.
- **The list.** `tools/list` attaches if it can, and lists the live registry, or the built-in one with the attachment's failure kept as a warning for the next result. The list last offered is remembered. On each attachment the live registry is compared with it; when the generated list differs, the adapter announces a change.
- **Watching.** In the live mode, the event watch starts on the first `subscriptions/listen` asking for `toolsListChanged`, or after `initialize`, and only in a typed profile. It is one thread, `luxforge-mcp-watch`, with its own connection to the session the call connection last attached to (`Target::again`), labelled "*client* via MCP (tool list)". It reads the event cursor and `schema.list`, then holds `events.wait` with the owner's 30 s bound, again and again; after a `flags.set` event, or a gap in the log, it reads `schema.list` again and compares. Its registry is adopted by the call connection before its next call, so a typed call uses the methods it announced. A watch that fails waits, without retrying, for the next attachment, and the next attachment replaces it. The headless target's `OwnCalls` starts no watch; `Owner::call`'s own re-read is compared instead.
- **Notifications.** A change is `notifications/tools/list_changed`: in the modern era once per open `toolsListChanged` subscription, tagged `_meta["io.modelcontextprotocol/subscriptionId"]` with that `subscriptions/listen` request's `id`, after its acknowledgement; in the legacy era once, without `params`, after the `initialize` answer. Nothing is sent before the client has been offered a list, nor when the generated list is the same.

### Warnings

A result's warnings are `ApiFailure`-shaped, `{code, message, job_id?, data?}`, in one trailing text block `{"warnings": [...]}` after the result and any pictures; `structuredContent` stays the method's result exactly. The codes are:

- `session-replaced`, `data: {catalog}`, on the first result after the adapter attached again.
- `job-released`, `data: {kind, status, ownership}`, when `job.read` says a job the call started belongs to its clients.
- `image-too-large`, `data: {field, path, bytes, limit}`, and `image-unreadable`, `data: {field, path}`, for a marked picture not attached.

### Identity

The label is the client's `clientInfo` title, else its name, with control characters removed and trimmed, shortened so that "*client* via MCP" is at most 64 characters. A client that gives neither is labelled "MCP client". The actor is `mcp:` and the name, cut at a character boundary to 128 bytes, or `mcp`. A modern request's own `clientInfo` names its actor; a legacy session's comes from `initialize`. The label is sent once, when the connection is made, from the request that attached.

### Connection loss

Before each call the adapter checks, without sending, whether the owner closed the idle connection. If it did, it attaches again before sending, so an edit after a desktop restart is applied normally with a `session-replaced` warning. A connection that fails while a call is on its way ends the session: a read reports `connection-lost`, and a mutation reports `outcome-unknown` with `data: {method, request_id}`. Neither is retried.

### Headless

`HeadlessTarget` holds the catalog, made absolute, the `luxforge-json` beside the adapter's executable (`current_exe`'s directory, with the platform's executable suffix; an executable that cannot be found ends the launch with `headless-failed` and exit status 1) and the configuration paths for the registry.

- **Attaching.** Each attachment first reads the registry of running live sessions. An entry whose catalog is the same path, or resolves to the same file, refuses `catalog-in-use` with `data: {catalog, pid, entry}`, and nothing is started. A registry that cannot be read refuses nothing and says so on standard error, since the catalog's lock still refuses a second owner. It then starts `luxforge-json --catalog PATH`, its three streams piped, and shakes hands within 30 s of the start: `schema.list` naming `luxforge-jsonl-1`, then `session.identify` with the label, as a live attachment does.
- **`headless-failed`.** A binary that cannot be started, or a child that ends, answers another protocol or does not answer within the handshake's 30 s, is stopped as below and reported once as `headless-failed`. Its message ends "it wrote: " and the child's standard error, and its `data` is `{catalog, binary, status, stderr, cause: {code, message}}`: `status` is how the child ended, `stderr` the last 8 KiB it wrote, and `cause` what the handshake saw. The catalog's lock refusing the child is `luxforge-json`'s own `{"error": {"code": "conflict", "message": "database is locked"}}` in `stderr`.
- **Threads.** Two named threads per child, each blocking on its pipe and ending with the child. One reads the answers, a line of at most the 64 MiB response bound each (a longer one is answered `resource-limit` and skipped to its end, so the next answer is read whole), into a channel of one. The other forwards the child's standard error line by line, prefixed `luxforge-json: `, and keeps its tail. The end of the answers is the child's end, which `closed` learns without sending; an answer that arrives with no call waiting also counts as closed, since the exchange is out of step.
- **Calls.** One JSON-lines request per call, without a token, its answer read with no deadline after the handshake, like a live call. A child that ends while a call is on its way is `connection-lost`, so `outcome-unknown` for a mutation ([connection loss](#connection-loss)).
- **Stopping.** Dropping the connection, when standard input ends, after a failed handshake, or when a lost session is ended, closes the child's standard input, waits up to 10 s for its answers to end, kills it if they have not, then waits for it and both threads. The child's start and end, with its process ID and exit status, go to standard error. A killed adapter leaves the child's standard input closed, so the child ends on its own.

### Tests

Unit tests cover framing, era and version rules, the label and actor, the queue's bound and both cancellations over a scripted handler, picture markers and the file bound, the compact tools over a scripted owner serving the built-in registry, the typed names (distinct and within 64 characters over the built-in and developer registries), annotations, namespaces and calls, list-change announcements to each era's listeners over a gated owner, the registry re-read after an only client's own `flags.set`, and the headless binary's place, catalog sameness and a binary that cannot start. The core's `agents` tests cover each client's text for the bundled path, each platform's file, headless and catalog arguments, and paths with spaces, quotes, backslashes and control characters read back by a JSON parser, a TOML parser and `/bin/sh`. The process tests (`tests/mcp/`) run the real binary against an in-process owner and `LocalServer`, with a proxy that cuts the connection after a forwarded edit and can hide the `history` methods from `schema.list`, which stands in for a flag that changes the registry, since none changes the live registry today. The typed process tests (`mcp_typed`) list without a session, filter by namespace, refuse an unknown namespace at launch, and in each era read, edit and receive the list change a person's `flags.set` announces. `mcp_interop` does the same through `rmcp` ([verification](#verification)). The headless process tests (`mcp_headless`) run it over the `luxforge-json` built beside it: an edit developed, made and read back, with the child gone and the catalog free once the input ends; a new child after the first is killed, with `session-replaced`; a catalog a registered session names refused before any child starts; and a child refused by the catalog's lock reported with what it wrote, once per call. `config` prints exactly the core's text for its own path for every client. Every message the binary writes there is validated against `tests/mcp/spec/`'s vendored `schema.json` of its era, checked by the request it answers. The vendored files' source, commit, checksums and licence are in `THIRD_PARTY.md` beside them.

## Verification

- **Schema generation.**
  - Every method of the full registry, and of the developer registry, generates a schema that validates against the JSON Schema 2020-12 metaschema.
  - Generated schemas accept the registry's own example requests.
  - Each kind's mapping has its own test.
  - Validation uses the pinned `jsonschema` crate as a development dependency.
- **Wire conformance.**
  - Every message the adapter writes in its tests validates against the vendored `schema.json` of its era (2026-07-28 or 2025-11-25), kept beside the tests with their source and licence.
  - Process tests cover:
    - discovery and the legacy handshake;
    - version refusal;
    - both list shapes;
    - calls, conflicts, a held job wait, images, a draft across calls;
    - cancellation, queue overflow and a lost connection;
    - no session, several sessions and headless.
  - These tests run the real binary against an in-process owner and `LocalServer`, as the `luxforge-ctl` process tests do.
- **Independent client.** `rmcp` 3.7.0, pinned as a development dependency of one integration test (`tests/mcp/interop.rs`), connects to the real binary with `--tools all` in both eras: 2026-07-28 through `server/discover` with a `toolsListChanged` subscription it checks is acknowledged, and 2025-11-25 through `initialize`. In each it lists the tools (checking one's title and annotations), calls `asset_state` and `edit_set-basic` (checking the revision and the history entry's actor), and receives the list change after a `flags.set`, on its subscription when modern and as a client notification when legacy, then lists the changed tools.
- **Rendered journey.** The `mcp` smoke scenario, in the rendered tier (`cargo xtask smoke --scenario mcp --output NEW_DIR`), drives the hidden, background-only desktop through the real `luxforge-mcp` built beside it:
  - **One launch, two clients.** The launch has an isolated data root and opens a synthetic RAW the run writes (frame 0 of `luxforge_testkit::hdr_bracket`'s still bracket, a RAW test camera, so the editor starts with `LUXFORGE_TEST_CAMERAS=1`) and then the generated 24 MP JPEG. The harness runs two adapters against its live session: `modern` speaks 2026-07-28 in the compact profile as Claude Code, and `legacy` speaks 2025-11-25 through `initialize` with typed tools (`--tools asset,edit,history,job,preview,session`) as Codex. The other two combinations are covered by the process tests.
  - **Each client.** Each lists its tools and reads both photographs' `asset.state`. It sets the JPEG's exposure (+1 EV and then −1 EV, the modern client passing the revision it read) and reads `history.list`, whose newest row has the actor `mcp:claude-code` or `mcp:codex` and is the entry the desktop shows. It fetches `preview.read`'s large tier, following its job with `job.wait`, and the attached JPEG must decode at 2048 px.
  - **The desktop.** It waits for each edit with the evidence step `{"usage": {"await": {"edited": true}}}`, which needs no statistics test eligibility when `edited` is its only condition, and captures it on screen, brighter and then darker than the opened JPEG. Settings › Agents lists "Claude Code via MCP", "Codex via MCP" and "Codex via MCP (tool list)" as `session.clients` answered.
  - **Disconnect.** The modern client's `events.wait`, for an event nothing makes, is sent before the legacy client starts, and the tab waits for the legacy client to be listed before it disconnects the modern one, so the call is held on the owner then. The tab's Disconnect then makes that held call report `connection-lost`; the client's next call attaches again with the `session-replaced` warning and is listed under a new client ID.
  - **Undo.** The desktop undoes both edits, through the modern client's entry back to the opened entry and picture. The originals' hashes are unchanged.
  - **Evidence.** It keeps every line written to and read from each adapter (`mcp-modern.jsonl`, `mcp-legacy.jsonl`), their diagnostics, exits and request IDs (`mcp.json`), the frames and `mcp-checks.json`. Every message an adapter wrote is held to the vendored `schema.json` of its era, as the process tests hold theirs, and `smoke --verify-only` replays the checks over a recorded run.
- **Real clients.** Qualification with Claude Code on the owner's M4 against a packaged build, set up from the tab's snippet: edit a photo, look at it, undo it in the desktop. It also covers whichever of Claude Desktop and Codex are installed, plus native Linux. This is recorded with its scope.

## Acceptance

- An MCP client configured from the tab or the guide lists Luxforge's tools without Luxforge open, attaches when it opens, and can do the following through either profile, each result preserving the API's structure:
  - read the catalog and a photo's state;
  - make a revision-checked edit that appears in history under its name and undoes like any other;
  - follow a job to its end;
  - see the photograph.
- Modern and legacy clients both work. An unsupported version is refused as the specification requires.
- No tool can do what a live session cannot: no permission authority, no consent grants, no direct catalog writes, no retries of mutations.
- Settings › Agents shows working configuration for the bundled binary and the connected clients, and Disconnect ends a client's session.
- `luxforge-mcp` ships in the macOS bundle and the Linux tarball beside `luxforge-ctl`, signed with the other helpers.
- The user guide documents only what exists.

## Decisions

Owner decisions, taken while planning:

- **Tools.** Both shapes from one generator, with the compact profile as the default. `--tools all`, or a namespace list, offers one typed tool per method.
- **Pictures.** Preview results carry the picture inline by default, at most the 2048 px tier and never a full-resolution export. `--no-images` withholds them.
- **Target.** The running desktop's live session by default. `--headless --catalog PATH` starts the adapter's own `luxforge-json` child, and is refused when a desktop has that catalog open.
- **Desktop.** A Settings › Agents tab with the binary's path, per-client configuration to copy, and the connected live clients.

Recorded defaults the owner can change; each is delegated to the implementer within the stated contract:

- stdio only, with no HTTP listener;
- dual-era support of 2026-07-28, 2025-11-25 and 2025-06-18;
- a hand-written protocol layer, with `rmcp` used only as a test client;
- tool names with `.` mapped to `_`;
- `expected_revision` optional for asset-revision methods;
- the actor `mcp:<client>` and the label "*client* via MCP";
- no resources, prompts or tasks;
- the 32-request queue and 8 MiB image bound;
- **Disconnect** on each connected client, with `session.disconnect` limited to permission authority;
- the snippet set: Claude Code, Claude Desktop, Codex, Cursor and generic JSON.

The exact module layout, thread structure, internal error types, `ttlMs` value, warning shapes, the wording of tool descriptions beyond the registry's notes, and the tab's layout within the Settings sheet's conventions are the implementer's.

## Later

- Publishing machine-readable shapes for the `json` parameters, and result schemas (`outputSchema`).
- The MCP tasks extension for Luxforge jobs.
- Streamable HTTP for remote or sandboxed agents, with its own authentication.
- A switch to turn live sessions off, and per-agent restrictions such as read-only agents.
