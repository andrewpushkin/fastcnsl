# AI assistant: Fix, chat and Autopilot

## Why

When a command fails (a forgotten `tar` flag, a wrong `git` option), the
user leaves the terminal to look up the answer. fastcnsl already knows which
lines are commands and what they printed, so it can hand that to an AI and
put the corrected command at the prompt. The same connection gives a chat
beside the terminal that sees what happens in it and can run commands.

The user brings their own API key for ChatGPT (OpenAI), Grok (xAI) or
Claude (Anthropic). fastcnsl has no account system and no server of its own.

## Goals

- **Options → AI**: choose a provider, give it a key, choose a model.
- **Fix**: one click beside a command sends the last few commands and their
  output to the AI. The corrected command appears at the prompt, not run,
  with a one-line explanation.
- **Chat**: a panel beside the terminal, per tab. The AI sees the tab's
  commands and output and proposes commands. The user types to the terminal
  or to the chat, whichever has the focus.
- **Autopilot**: a switch in the chat panel. Off: every command the AI
  proposes waits for the user. On: the AI runs commands and reads their
  output by itself until the task is done or the user stops it.
- The terminal never freezes while the AI is working, and stays as fast as
  it is today when the AI is not used.

## Non-goals

- Logging in with a Claude subscription, or reading Claude Code's stored
  sign-in: Anthropic forbids other apps to "collect, store, or intermediate
  Claude.ai credentials". ChatGPT (stage 1c) and Grok (stage 1d) have
  sign-ins; a Claude plan is used only through the user's own Claude Code
  (stage 1e).
- Suggestions while typing (autocomplete), or anything sent without a click.
- Saving chats to disk. A chat lives as long as its tab.
- Full Markdown, images or file attachments in the chat.
- Selecting chat text with the mouse (each message and command has Copy).
- Shell integration (exit codes, exact command end). See "Later".
- Hiding secrets in the text that is sent. See "Privacy".

## Anti-goals

- Anything is sent to a provider that the user did not ask to send.
- A command runs that the user did not approve, while Autopilot is off.
- An API key ends up in `config.toml`, in a log file or in the debug log.

## Constraints

- The app has no network code, no async runtime and few dependencies. The
  window and all tabs run on one event loop. Other threads report through
  `UserEvent` (`src/session.rs`).
- Commands are known only by the line Enter was pressed on
  (`LineIndex::on_enter`, `src/lines.rs`). There is no exit code and no
  signal that a command has ended.
- There is no split view. One tab shows one terminal grid across the window.
- The Options panel has buttons and lists but no typed text field. The
  search bar is the only text input.
- Windows, Linux and macOS, x86-64 and ARM64, all built in Docker from Linux.

## Approaches considered

**Talking to the providers**

1. **Two built-in clients (chosen).** One for the Chat Completions format
   (OpenAI, xAI, and any server with the same format, such as a local
   Ollama). One for Claude's Messages API. Each is a few hundred lines over
   a blocking HTTPS library, on its own thread.
2. One Chat Completions client for all three, with Claude through
   Anthropic's compatibility layer. Less code, but that layer is not the
   supported way to use Claude and lacks its features.
3. Running installed tools (`claude`, `codex`) as helpers. No network code
   in fastcnsl and subscriptions would work, but it needs those tools
   installed, has nothing for Grok, and is awkward on Windows.

**Knowing when a command has finished (for Autopilot)**

1. **Output goes quiet (chosen).** Works in every shell and over ssh. A long
   silent command can look finished, so the AI can ask to wait longer.
2. Shell integration (OSC 133 marks). Exact, with exit codes, but needs a
   snippet in each shell's startup file and does not reach `cmd` or remote
   hosts. It can be added later on top of 1.

**Order.** Three stages, each released on its own: (1) Options → AI and
Fix, (2) the chat panel, (3) Autopilot.

## Design

### Settings and keys

`config.toml` gets an `[ai]` table:

```toml
[ai]
provider = ""          # "openai", "xai", "anthropic" or "custom"; "" = AI off
model = ""             # as the provider names it
base_url = ""          # "custom" only, e.g. "http://localhost:11434/v1"
listed_commands = 50   # recent commands listed to the AI (lines only)
panel_width = 0.4      # chat panel's share of the window
```

Keys are not in `config.toml`, because people share that file. They are in
`ai-keys.toml` beside it (so the portable build keeps them in its folder),
created readable by the user only, as `src/remote.rs` does for its instance
files. One key per provider, so switching providers keeps each key. An
environment variable (`OPENAI_API_KEY`, `XAI_API_KEY`, `ANTHROPIC_API_KEY`)
is used when the file has no key for that provider.

**Options → AI** (a fifth page):

- Provider: a row of buttons.
- Key: shown as `sk-…a1b2` or "not set". **Paste key** takes it from the
  clipboard; **Remove** deletes it. No text field is needed.
- Model: a list fetched from the provider's `GET /v1/models` once a key is
  set (both formats have it). For Claude the default is `claude-opus-5-5`.
  For the others the user picks; there is no built-in default to go stale.
- **Test**: sends a one-word request and shows "works" or the error.
- A line saying what is sent and when (see "Privacy").

With no provider set, the AI menu items are shown and open this page.

### Where the user starts it

- **Left-click the gutter** beside any line: a menu for the command that
  line belongs to, with **Fix this command** first, the bookmark toggle,
  and (from stage 2) **Ask AI in chat**. (Before, a left click on the
  gutter did nothing.)
- The same two items in the terminal's right-click menu, and **AI chat** in
  the ⋮ menu.
- **Open AI chat** in a tab's right-click menu (the menu on the tab
  itself), opening the chat panel for that tab.
- Shortcuts, changeable like the others: `ai_fix` (the last command) and
  `ai_chat` (open the panel or move the focus between it and the terminal).

### What is sent: a list first, output on request

Like skills: the AI gets a short list up front and asks for the details it
needs. Every line is named by its number in the tab, the number the gutter
shows in "Per tab" mode, so the AI, the user and the bookmarks share one
numbering.

**Sent with every request:**

- The shell's name, the operating system and the tab's program name.
- The list of the last `listed_commands` commands, newest last: the
  command's line number, its time of day, the command line as shown (with
  the prompt: fastcnsl can't tell them apart, and the AI is told so), how
  many lines it printed, and the numbers of its bookmarked lines.
- The text of the tab's bookmarked lines (the 20 most recent), each with
  its number and the command it belongs to.
- **The output of the last command, always**, with numbered lines; and the
  output of the clicked command too when Fix is for an older one. Long
  output comes as its first 50 and last 150 lines with a line saying how
  many were left out.

```
Commands in this tab (newest last):
  line 1    17:31  dev$ make              2340 lines  bookmarked: 812
  line 2342 17:38  dev$ echo hello           1 line
  line 2344 17:38  dev$ tar -xf a.tgz -C     2 lines  <- fix this one
Bookmarked lines:
  812 (in line 1): src/x.c:41: error: 'n' undeclared
--- output of line 2344 ---
2345 | tar: option requires an argument -- 'C'
2346 | Try 'tar --help' or 'tar --usage' for more information.
```

A bookmarked line in sent output is marked `*` after its number.

**Tools the AI has:**

- `get_output(line, from?, to?)`: the output of the command on `line`
  (numbered). Without `from`/`to`: all of it up to 200 lines, else the
  first 50 and last 150. With them: that range of the tab's lines, at most
  300.
- `add_bookmark(line)`: bookmarks a line the tab has (any line up to the
  prompt). Bookmarks it sets look like the user's.
- `propose_command(command, explanation)`: ends the Fix (below).

**Limits.** At most 6 requests per Fix; a seventh tool call ends it with
"the AI took too many steps". Every request carries the whole conversation
so far, so a Fix sends at most about 120 000 characters of terminal text in
all; tool results past that say "limit reached". Tool calls are answered on
the UI thread (the worker asks through `UserEvent` and waits on a channel,
as `remote.rs` does), so terminal text is read only where it is today.

### Fix

1. Build the first request and send it on a new thread. A notice
   ("Asking Claude… Esc cancels") stays up until the answer and says what
   the AI is reading ("reading the output of line 1"). Esc cancels.
2. The AI may call `get_output` and `add_bookmark`, then answers through
   `propose_command(command, explanation)`, or with a sentence if there is
   nothing to fix.
3. The command is written to the terminal as a paste without the final
   newline, so it sits at the prompt for the user to read, edit and run.
   Of a multi-line answer only the first command is used (lines ending in
   `\`, `&&`, `||` or `|` continue it). The explanation shows in the
   notice at the bottom of the window, followed by "Bookmarked line N"
   for each bookmark the AI set.

Fix refuses with a notice when a full-screen program (alt screen) is open.
If the AI has no command to offer, only the explanation is shown.

### ChatGPT sign-in (stage 1c)

OpenAI's "Sign in with ChatGPT" lets an open-source app spend a ChatGPT
Plus or Pro plan instead of an API key (docs:
developers.openai.com/siwc/token-sharing-open-source). fastcnsl is MIT on
GitHub, so it uses the open-source route: no approval, dynamic client
registration.

**Options → AI, provider ChatGPT** gets an *Account* row above the key:
**Continue with ChatGPT** (plain text; the panel draws no images), or once
signed in `you@example.com · Using ChatGPT plan` with **Manage usage**
(opens chatgpt.com/settings/usage) and **Sign out**. When signed in, the
plan is used and the API key (if any) is not. The first successful sign-in
shows a one-time panel "You're using your ChatGPT plan" (Got it), as
OpenAI's guidelines ask.

**Sign-in** (a thread, `AiEvent::SignedIn` when done):

1. Listen on `127.0.0.1:1455` (another free port if taken; only the port
   may change), path `/auth/callback`.
2. Open in the browser `https://auth.openai.com/api/accounts/authorize`
   with `response_type=code`, `client_id` (`dynamic_agent_client` the first
   time, with `agent_name_hint=fastcnsl`; the issued one after),
   `ext_agent_host_id` (`urn:uuid:<random>`, made once and kept),
   `redirect_uri`, `scope=openid profile email offline_access
   resource.invoke chatgpt.tokens.use.direct`,
   `resource=https://api.openai.com/v1`, `state`, `nonce`,
   `code_challenge` (S256 of a random verifier), and `id_token_hint` when
   a previous ID token is kept.
3. The callback brings `code`, `state` and (first time) `client_id`.
   Wrong `state`, `error=…`, or a `client_id` different from the saved one
   end the sign-in. The browser gets a short "You can close this tab" page.
4. `POST https://auth.openai.com/api/accounts/oauth/token` (form):
   `grant_type=authorization_code`, the issued `client_id`, `code`,
   `code_verifier`, `redirect_uri`, `resource`.
5. The granted scope must include `chatgpt.tokens.use.direct`, else
   "this ChatGPT plan can't be used by other apps". The ID token's payload
   gives the email and must carry our `nonce`. Its signature is not
   checked: fastcnsl uses it only to show the email and as a hint, never
   to authorise anything.
6. Saved to `chatgpt-login.json` beside `config.toml` (written whole, then
   renamed; 0600 on Unix): `client_id`, `ext_agent_host_id`, `email`,
   `id_token`, `access_token`, `refresh_token`, `expires_at`, `scopes`,
   `welcomed`.

Five minutes without a callback, or a new sign-in, ends a waiting one.

**Requests.** `POST https://api.openai.com/v1/responses` with
`Authorization: Bearer <access token>`, `instructions` (the system
prompt), `input`, function `tools`, `store: false`, `stream: true`; no
token limit or sampling fields. The stream is read to `response.completed`
(its `response.output` holds the text and the `function_call` items);
`response.failed` and `response.incomplete` are errors. Function calls go
back as `function_call` items without their `id` (nothing is stored), with
`function_call_output` items after them; reasoning items are not sent back.
Models: `GET /v1/models`, those with `visibility == "list"`.

**Tokens.** The access token lasts an hour. Within a minute of expiry,
before a request, it is refreshed (`grant_type=refresh_token`, issued
`client_id`, `refresh_token`, `resource`), one refresh at a time; the new
tokens replace the old together. A refresh error such as `invalid_grant`
clears the tokens: "Sign in to ChatGPT again (Options → AI)".

**Errors** (`error.code`): `subscription_sharing_user_not_eligible` → "your
ChatGPT plan can't be used here (Plus or Pro is needed)", no retry;
`subscription_sharing_usage_limit_exceeded` → "you have reached your
ChatGPT plan's limit; see Manage usage"; the two `…_unavailable` → "ChatGPT
is busy, try again"; `subscription_sharing_invalid_user` → sign in again.

**Sign out** stops using the tokens, sends the refresh token to the
`revocation_endpoint` from `https://auth.openai.com/.well-known/openid-configuration`
(form: `token`, `token_type_hint=refresh_token`, `client_id`) in the
background, and clears the tokens. The client id and host id stay.

**Testing.** Debug builds read `FASTCNSL_CHATGPT_AUTH` and
`FASTCNSL_CHATGPT_API` to point at a fake server; release builds ignore
them.

### Grok sign-in (stage 1d)

xAI lets other apps spend a SuperGrok or X Premium plan (it announced
OpenCode; Hermes Agent, Kilo, Warp and OpenClaw do the same). There is no
registration for apps: all of them use xAI's shared client, the Grok Build
CLI's id `b1a00492-073a-47ea-816f-4c329264a828`, and say who they are with
`referrer` (xAI's own code: "so analytics can attribute OAuth usage"). So
xAI's page names the app "Grok Build"; Options says so. xAI may close the
shared client to other apps; the API key stays the fallback.

**Approaches.** (a) Device code (RFC 8628), as OpenCode and Hermes do:
fastcnsl shows a code, opens xAI's page with it filled in, and polls.
(b) Browser redirect to a loopback port: OpenCode dropped it because xAI
sometimes shows a code instead of redirecting, and its fixed port clashes
with Grok Build. (c) Running Grok Build itself over ACP (T3 Code): fully
official, but needs Grok Build installed and is an agent, not a model API.
Chosen: (a).

**Options → AI, provider Grok** gets an *Account* row: **Continue with
Grok** ("uses your SuperGrok or X Premium plan"); while waiting `Enter
code ABCD-1234 at accounts.x.ai` with **Open page** and **Cancel**; signed
in `you@example.com · Using Grok plan` with **Sign out**. A dim line below
says "xAI's page names this app Grok Build (xAI's shared sign-in)".

**Sign-in** (a thread; `AiEvent::DeviceCode` once the code is known,
`AiEvent::SignedIn` when done):

1. `POST https://auth.x.ai/oauth2/device/code` (form): `client_id`,
   `scope=openid profile email offline_access grok-cli:access api:access`,
   `referrer=fastcnsl`. The answer has `device_code`, `user_code`,
   `verification_uri`, `verification_uri_complete`, `expires_in` (1800),
   `interval` (5).
2. Open `verification_uri_complete` (else `verification_uri`) in the
   browser, and show the code.
3. Every `interval` seconds `POST https://auth.x.ai/oauth2/token`:
   `grant_type=urn:ietf:params:oauth:grant-type:device_code`, `client_id`,
   `device_code`. `authorization_pending` waits; `slow_down` adds 5 s;
   `access_denied` is "sign-in was cancelled"; `expired_token` or
   `expires_in` passing is "the code expired"; Cancel stops at once.
4. The email comes from the ID token, else from
   `https://auth.x.ai/oauth2/userinfo`; none is fine ("Signed in").
5. Saved to `grok-login.json` beside `config.toml` (written whole, then
   renamed; 0600): `email`, `access_token`, `refresh_token`, `expires_at`.

**Requests.** `POST https://api.x.ai/v1/responses` with `Authorization:
Bearer <access token>`, the same body and stream reading as ChatGPT's.
Models: `GET /v1/models`. A 401 refreshes the token and tries once more. A
403 is the plan gate: "xAI didn't allow your Grok plan here (SuperGrok or
X Premium is needed); you can use an API key instead".

**Tokens** last about six hours. Within five minutes of expiry, before a
request, they are refreshed (`grant_type=refresh_token`, `client_id`,
`refresh_token`), one at a time. `invalid_grant` (or another 400/401)
clears them: "sign in again (Options → AI)". A 403 keeps them and says
the plan text above.

**Sign out** posts `token`, `token_type_hint=refresh_token`, `client_id`
to `https://auth.x.ai/oauth2/revoke` in the background and clears the file.

**Testing.** Debug builds read `FASTCNSL_GROK_AUTH` and `FASTCNSL_GROK_API`.

### Claude Code (stage 1e)

A provider "Claude Code" (`provider = "claude-code"`) runs the user's own
installed, unmodified Claude Code in print mode for each request. Claude
Code signs in by itself (a Pro or Max plan, or its own key); fastcnsl never
reads, keeps or passes on its credentials.

**Approaches.** (a) Read Claude Code's stored OAuth token and call the API:
forbidden (above). (b) Run `claude -p` per request, stateless, the
conversation written out as one prompt, the answer as JSON in a schema of
tool calls: works through WSL with only stdin and stdout. (c) Run `claude
-p` once with fastcnsl's tools as an MCP server on localhost: one process
per Fix, but WSL 2 can't reach Windows' localhost by default. Chosen: (b).
Anthropic's terms allow "an end user signing in to the unmodified Claude
Code binary with their own Claude subscription" but say nothing about an
app running it; the Options text says what happens.

**Finding it.** `claude` (`claude.exe`) on PATH or in `~/.local/bin`; on
Windows, else in the default WSL distribution (`wsl.exe -e sh -lc 'command
-v claude'`, or `~/.local/bin/claude`). Found on the request's thread,
kept once found.

**A run.** `claude -p --output-format json --tools "" --strict-mcp-config
--no-session-persistence --system-prompt <short> [--model M] [--json-schema
S]`, in the temp folder (WSL: `/tmp`, through `sh -lc` so an npm install
finds node). The prompt on stdin: the Fix's instructions, each tool with
its input schema, then the conversation (`=== From the terminal ===`, `===
Your answer ===` with the calls, `=== Result of get_output ===`). The
schema: `{"text": string, "calls": [anyOf per tool {name, input}]}`;
`structured_output` becomes the Reply. `is_error` with "Not logged in" →
"Claude Code is not signed in: run claude in a terminal and sign in"; 429
or "limit reached" → rate limited. Five minutes is the limit of one run.

**Options → AI, Claude Code**: no key row; *Account* says "your installed
Claude Code and its sign-in (Pro or Max plan)" and that fastcnsl runs
`claude -p` and never sees the sign-in. Models: `sonnet` (default),
`opus`, `haiku`, Claude Code's aliases. Test sends one tiny request.

### The chat panel

- Per tab, on the right, `panel_width` of the window, with a divider that
  can be dragged. The terminal grid gets narrower and the shell is told the
  new size, as on a window resize. Closing the panel gives the width back.
- Header: provider and model, tokens used by this chat, **Autopilot**
  switch, **New chat**, close.
- Messages scroll. Text is wrapped plain text; fenced code is shown in the
  terminal font on a tinted background. Each message has Copy.
- A command the AI proposes is a card with the command and two buttons:
  **Insert** (put it at the prompt, as Fix does) and **Run** (insert and
  press Enter). The card then shows "running", "done" or "skipped".
- Input box at the bottom: Enter sends, Shift+Enter makes a new line, paste
  works. Esc stops a reply that is streaming.
- Focus: a click or `ai_chat` moves it. The side with the focus has the
  caret; the other side's caret is hollow. Terminal shortcuts (tabs, find,
  copy) work from either side.

**What the AI knows.** As in Fix: every message carries the list of
recent commands and bookmarked lines and the last command's output (for
"Ask AI in chat", the clicked command's too); other output on request
with `get_output`. It can bookmark lines with `add_bookmark`.

**One loop for both modes.** In chat the AI also has
`run_command(command)`. Its result is the command's output.

- Autopilot off: the tool call becomes a card. Run, or Insert followed by
  the user's own Enter (they may edit the command first), runs it; the
  result is what was actually entered and what it printed. Typing a new
  message instead answers the call with "the user did not run this".
- Autopilot on: the same call runs at once.

The conversation is kept in memory and only ever appended to. When it
nears the model's limit the panel says so and offers New chat; nothing is
trimmed. For Claude, replies are stored and sent back exactly as received.

### Autopilot

Off by default, per chat, and off again after New chat. While it is on,
the switch and the tab's label are marked in the warning colour.

A command the AI runs is written to the terminal with Enter, so it is seen
in the scrollback, gets a command mark and time, and is logged like any
other.

**When is it finished?** Output has been quiet for 1.5 s. After 30 s
without a quiet moment the result is the output so far with "still
running". A second tool, `wait(seconds)` (at most 60), returns whatever
appeared meanwhile. A third, `send_keys(text)`, answers a prompt such as
`[Y/n]` or sends Ctrl+C.

**Stopping.** Any of these ends the AI's turn and switches Autopilot off:
the Stop button, Esc in the panel, the user typing in the terminal, a
full-screen program opening, 25 commands since the user's last message
(the panel asks whether to go on), closing the panel or the tab.

The AI is told never to type passwords: at a password prompt it stops and
asks the user.

There is no list of forbidden commands. Such a list is easy to get around
and gives false comfort. Autopilot is a plain choice to let the AI act.

### Clients (`src/ai/`)

- A common interface: send a conversation with tools, get back a stream of
  events (text, tool call, usage, end, error) through `UserEvent::Ai(tab,
  …)`. A cancel flag is checked between chunks.
- **Chat Completions client**: `POST {base}/chat/completions`, streaming,
  function tools. Bases: `https://api.openai.com/v1`, `https://api.x.ai/v1`,
  or `base_url`.
- **Claude client**: `POST https://api.anthropic.com/v1/messages`,
  streaming, written from Anthropic's current HTTP documentation and not
  from memory, because recent models changed what is accepted. Known today:
  tool choice stays automatic (forcing a tool is rejected), thinking and
  sampling settings are left out, the `refusal` and `max_tokens` stop
  reasons are handled and shown, and the conversation is cached.
- New dependencies: a blocking HTTPS client with rustls (`ureq` is the
  intended one) and `serde_json`. No async runtime. Expect the binary to
  grow by one to two megabytes.
- Errors are shown where the request started (notice for Fix, a message in
  the chat): wrong key, rate limit, no network, refusal. One automatic
  retry for a dropped connection, none otherwise.

### Privacy and safety

- Requests are made only by Fix (and the tool calls inside one), a chat
  message, an Autopilot step, the model list and Test.
- The first Fix or chat message with a provider shows, once, what is sent
  (the list of recent commands, bookmarked lines, the last command's
  output, and any other output the AI asks for) and asks to go on.
- Nothing is removed from the text. A password or token printed in those
  lines is sent. This is said in Options → AI and in the README.
- Terminal output can contain text written to steer the AI (a file that
  was `cat`, a web page that was `curl`ed). It is sent as tool results and
  the AI is told to treat it as data. With Autopilot on this remains a
  real risk, and the README says so.
- Keys never go to the log; the debug log records requests without headers
  or bodies.

### Tests

- Context builder: with `TestTerm`, as `src/lines.rs` tests do (the
  command list, numbered output, bookmarks, the caps, a command clicked in
  the middle).
- Clients: recorded streaming replies as fixtures; a local `TcpListener`
  playing the provider for a whole Fix and a whole tool loop.
- "Finished" detection: with a fake clock.
- Layout: the panel, cards and the Options page, as `src/view.rs` and
  `src/chrome.rs` test theirs.
- No test calls a real provider. A manual check with a real key per
  provider before each stage is released.

## Stages

1. **Fix**: `[ai]` settings, key file, Options → AI, both clients, the
   context builder, the gutter and right-click items, Fix. (Built first
   with all output sent up front.)
1b. **Fix with a list and tools**: the command list, bookmarks, the tool
   loop (`get_output`, `add_bookmark`) in both API formats. The chat
   reuses all of it.
1c. **ChatGPT sign-in**: the plan instead of an API key, through the
   Responses API (above).
1d. **Grok sign-in**: a SuperGrok or X Premium plan, by device code.
1e. **Claude Code**: a Claude plan through the user's own `claude -p`.
2. **Chat**: the split and resize, the panel, focus, cards with Insert and
   Run, the `run_command` loop with Autopilot fixed off.
3. **Autopilot**: the switch, "finished" detection, `wait` and
   `send_keys`, the stop rules.

README and CHANGELOG are updated with each stage.

## Left to the builder

- Default keys for `ai_fix` and `ai_chat` that are free on all three
  systems.
- The wording of the instructions given to the AI, and the tool schemas.
- How a line already typed at the prompt is handled before Insert (cleared
  per shell, or appended to).
- Exact crate versions and TLS root certificates (bundled or the system's).
- Panel visuals within the existing palette.

## Later

- Shell integration for exact command ends and exit codes, which would also
  let the gutter mark failed commands and offer Fix on them.
- Saved chats, more providers, a per-tab choice of model.
