# AI stage 3: Autopilot — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** An optional Autopilot switch in the chat panel: while it is on, the AI runs the commands it proposes by itself, reads their output when it goes quiet, can wait longer or send keys, and the panel shows each command it ran with the end of its output.

**Architecture:** The request loop in `src/ai/chat.rs` gains a `Driver`: with Autopilot on, `run_command`, `wait` and `send_keys` go to the driver instead of becoming cards. "Finished" detection (quiet for 1.5 s, else "still running" after 30 s) lives in the worker as a pure function over the driver, so it is tested with a fake clock. The real driver sends `AiEvent::Drive` requests to the UI thread, which types the command (as Run does), answers polls with the command's output and how long the tab has been quiet, and turns a stop (Esc, Stop, typing in the terminal, a full-screen program, closing the panel, switching Autopilot off) into a `Stopped` answer that ends the AI's turn gracefully.

**Tech Stack:** as stage 2.

**Spec:** `docs/superpowers/specs/2026-11-13-ai-assistant-design.md`, section "Autopilot" and stage 3 under "Stages". Builds on stage 2 (`docs/superpowers/plans/2026-11-14-ai-chat.md`, done).

## Global Constraints

- Off by default, per chat; `New chat` turns it off. The switch reads `Autopilot off` / `Autopilot on`; on, it and the tab's label are drawn in the warning colour (`palette.ansi[3]`).
- The setting is read when a message is sent: switching it on affects the next message; switching it off during a turn stops the turn.
- A command the AI runs is typed with Enter exactly as Run does (`insert_command` + `note_enter` + `\r`): it gets a command mark and time and is logged.
- Finished: no output for `QUIET = 1.5 s`; after `RUN_MAX = 30 s` without that, the result is the output so far marked "still running". Polls every `POLL = 250 ms`.
- `wait(seconds)`: waits `min(seconds, 60)` s, then returns the last command's output so far. `send_keys(text, enter, ctrl_c)`: types `text`, then Enter if `enter`, Ctrl+C if `ctrl_c`, then waits as for a command.
- Stops (each ends the turn and switches Autopilot off, with a Note saying why): Stop button or Esc in the panel ("you stopped it"), typing or pasting in the terminal ("you typed in the terminal"), a full-screen program ("a full-screen program opened"), `MAX_COMMANDS = 25` commands since the user's message ("it ran 25 commands; send a message to let it go on"), switching it off ("Autopilot was switched off"), closing the panel ("the chat was closed"). Closing the tab ends it with no note. A second Esc while stopping drops the turn at once.
- The AI is told never to type passwords and to stop and ask at a password prompt. There is no command blocklist.
- Requests per message with Autopilot: `AUTOPILOT_REQUESTS = 40` (8 without).
- Output sent to the AI goes through a per-message `Budget` (120 000 bytes), as Fix's.
- Build/test: `export PATH="$HOME/.cargo/bin:$PATH"`; `cargo test`, `cargo clippy --all-targets -- -D warnings` clean after each task.

## Review Focus

1. The AI calls `run_command` twice in one reply, the first one is stopped → the second is answered "not run: stopped", every call has exactly one result (Task 1 `a_stop_answers_every_call`).
2. A command that prints forever (`yes`, a build) → the result comes after 30 s as "still running", not never (Task 1 `endless_output_is_still_running_after_30_s`).
3. A command that prints nothing (`sleep 5`) → it reads as finished after 1.5 s; the AI can call `wait` (Task 1 `silence_is_finished_after_1_5_s`, `wait_waits_the_whole_time`).
4. The user types in the terminal while the AI waits on a command → the turn ends with a Note, Autopilot is off, the keys reach the shell (Task 3 manual).
5. New chat or closing the tab mid-turn → no command is typed afterwards, no panic (Task 3 manual).

---

### Task 1: The Autopilot loop and "finished" detection

**Files:** Modify `src/ai/chat.rs`, `src/ai/mod.rs`.

**Interfaces:**
- Produces in `chat.rs`:

```rust
pub const QUIET: Duration = Duration::from_millis(1500);
pub const RUN_MAX: Duration = Duration::from_secs(30);
pub const WAIT_MAX: u64 = 60;
pub const POLL: Duration = Duration::from_millis(250);
pub const MAX_COMMANDS: usize = 25;
pub const AUTOPILOT_REQUESTS: usize = 40;
pub const SYSTEM_AUTOPILOT: &str; // SYSTEM plus: Autopilot is on; run_command runs at once and returns the output (finished when quiet 1.5 s, "still running" after 30 s); wait to wait longer; send_keys for prompts or Ctrl+C; never type a password: stop and ask; one step at a time.

/// Requests to the terminal, answered on the UI thread.
#[derive(Debug, Clone, PartialEq)]
pub enum Drive {
    /// Type `command` and Enter; `said` is what the AI wrote before it (shown first).
    Run { command: String, explanation: String, said: Vec<String> },
    Keys { text: String, enter: bool, ctrl_c: bool },
    /// The output of the command on global line `line` (None: the last one) and how long the tab has been quiet.
    Poll { line: Option<u64> },
}
#[derive(Debug, Clone, PartialEq)]
pub enum Driven { Ran { line: u64 }, Sent, Output { text: String, quiet: Duration }, Stopped(String) }

pub trait Driver {
    /// None: the turn was dropped (cancelled).
    fn drive(&mut self, d: Drive) -> Option<Driven>;
    fn sleep(&mut self, d: Duration);
    fn now(&self) -> Instant;
}

/// Polls `line` until quiet for QUIET or `max` passed: (output, still running), or the stop reason.
pub fn wait_for_quiet(driver: &mut dyn Driver, line: Option<u64>, max: Duration) -> Result<Result<(String, bool), String>, AiError>;
pub fn tools(autopilot: bool) -> Vec<Tool>; // + wait{seconds: integer}, send_keys{text, enter, ctrl_c} when autopilot
pub fn run(client: &Client, model: &str, history: Vec<Value>, autopilot: Option<&mut dyn Driver>, ask: impl FnMut(Vec<ToolCall>) -> Option<Vec<String>>) -> Result<Turn, AiError>;
```

- `Turn` gains `stopped: Option<String>`; `Turn.texts` holds only texts not already passed in a `Drive::Run`.
- `AiEvent::Drive { job: u64, tab: u64, request: chat::Drive, reply: Sender<chat::Driven> }`; `chat::start` takes `autopilot: bool` and builds a channel driver (`std::thread::sleep`, `Instant::now`).
- Results (exact): run → `ran on line {line+1}; output:\n{text}`, or `ran on line {n}; still running after 30 s; output so far:\n{text}`; wait → `output so far:\n{text}`; send_keys → `sent; output:\n{text}` (same "still running" form); stopped → `stopped: {reason}`, later calls in the batch `not run: stopped`. No command yet for `wait`/`send_keys` → Poll with `line: None` (the last command).

- [ ] **Step 1: Failing tests** with a `FakeDriver { now: Instant, outputs: VecDeque<Driven>, log: Vec<Drive> }` whose `sleep` advances `now`:
  - `silence_is_finished_after_1_5_s`: polls answer `quiet` growing with the clock → `Ok(Ok((text, false)))` after ~1.5 s of fake time.
  - `endless_output_is_still_running_after_30_s`: polls always `quiet: 0` → `Ok(Ok((text, true)))` once 30 s passed.
  - `a_stop_while_waiting_is_returned`: a `Stopped("x")` poll → `Ok(Err("x"))`.
  - `autopilot_runs_the_command_and_goes_on` (serve RUN then TEXT): the driver gets `Run{command:"ls -la", said:["Try this."]}`, the second request carries `ran on line 10; output:`, the turn has no proposals and `texts == ["Done."]`.
  - `a_stop_answers_every_call`: two run_command calls in one reply, `Run` answered `Stopped("you stopped it")` → no second request is made, `turn.stopped == Some("you stopped it")`, the last messages hold `stopped: you stopped it` and `not run: stopped`.
  - `wait_waits_the_whole_time`: `wait{seconds: 90}` → fake clock advances 60 s, result `output so far:`.
  - `send_keys_types_and_waits`: driver gets `Keys{text:"y", enter:true, ctrl_c:false}`.
  - `the_25th_command_is_the_last`: 26 RUN replies → the 26th is not driven, `stopped` names 25 commands.
  - `autopilot_offers_wait_and_send_keys`: `tools(true)` names end with `wait`, `send_keys`; `tools(false)` has 3.
- [ ] **Step 2:** `cargo test ai::chat` → fail.
- [ ] **Step 3:** Implement.
- [ ] **Step 4:** `cargo test` green; clippy clean.
- [ ] **Step 5:** Commit `AI autopilot: run commands, wait for quiet output, stop gracefully`.

### Task 2: The chat and the panel know Autopilot

**Files:** Modify `src/ai/chat.rs`, `src/chat_ui.rs`.

**Interfaces:**
- `Chat` gains `pub autopilot: bool`, `pub stop: Option<String>` (a stop asked for, answered at the next drive), `pub driving: bool` (the turn in flight runs with Autopilot). `Card` gains `pub output: Vec<String>` (the last lines of a command the AI ran). `CardState::Auto { line: u64, running: bool }`; `ask` leaves Auto cards alone; `on_turn` pushes a Note `Autopilot stopped: {reason}` when `turn.stopped`, marks Auto cards not running.
- `ChatHit::Autopilot`, `ChatHit::Stop`. Header right to left: `×`, `New chat`, `Autopilot off`/`Autopilot on`; `Stop` replaces `New chat` while a reply is awaited. Auto cards draw the command, up to 4 output lines (terminal font, dim) and `running…` / `done`; no buttons.
- `ChatLayout` gains `autopilot: bool` (the switch's colour).

- [ ] **Step 1: Failing tests:** `the_switch_is_in_the_header` (hit Autopilot; label follows `chat.autopilot`), `stop_shows_while_waiting` (job set → Stop button, no New chat), `an_auto_card_shows_its_output` (lines contain the last 4 output lines and `running…`, no Insert/Run), `ask_leaves_auto_cards_alone`, `new_chat_turns_autopilot_off`, `a_stopped_turn_leaves_a_note`.
- [ ] **Step 2–4:** fail, implement, green + clippy.
- [ ] **Step 5:** Commit `AI autopilot: the switch, Stop, and the commands it ran`.

### Task 3: The UI drives the terminal

**Files:** Modify `src/app.rs`, `src/view.rs`.

**Interfaces / behaviour:**
- `Tab` gains `last_output: Instant` (set on `TermEvent::Wakeup`) and the chat's `auto_since: Instant` (when the AI last typed).
- `send_chat`: `chat.driving = chat.autopilot`; `chat::start(.., autopilot)`. Status while driving: `Autopilot is working… Esc stops`; with a stop pending: `Stopping…`.
- `AiEvent::Drive`: tab and job must match (else drop `reply`). `chat.stop` set or `!chat.autopilot` → `Stopped(reason)`. Alt screen → stop "a full-screen program opened". `Run` → push `said` as Ai entries, type it like Run, push an Auto card, `Ran{line}`; `Keys` → write, `Sent`; `Poll` → `context::output` of the command (TabText), quiet = now − max(last_output, auto_since); update the card's last 4 lines (text after `| `). Every Drive marks earlier Auto cards not running.
- `fn stop_autopilot(&mut self, i: usize, reason: &str)`: `autopilot = false`; if `driving` and job set → `stop = Some(reason)`. Called from: Esc / Stop with `driving` (a second Esc or Stop while `stop` is set drops the job as today), the switch turned off, close_chat, typing in the terminal (`on_key` terminal path) and pasting there.
- Tab label in the warning colour while its chat has Autopilot on (`ChromeView.autopilot: Vec<bool>`); the switch's button too.
- ChatTurn clears `stop`, `driving`.
- [ ] **Step 1: Failing test** in `view.rs`: `an_autopilot_tab_is_marked` — a tab with autopilot draws its title glyphs in `ansi[3]`.
- [ ] **Step 2–4:** fail, implement, green + clippy.
- [ ] **Step 5: Manual check** (Docker Xvfb + fake server that answers `run_command` then text): turn on, ask, the command runs by itself with its output in the card and the AI's follow-up; `sleep 3` reads finished at 1.5 s; `yes` → "still running" after 30 s (Ctrl+C via a second round); Esc mid-wait → Note, switch off; typing in the terminal mid-wait → Note; New chat mid-turn → nothing typed later.
- [ ] **Step 6:** Commit `AI autopilot: the terminal runs what the AI asks, until it is stopped`.

### Task 4: Documentation

- [ ] README "Chat with AI about a tab": an Autopilot paragraph (what it does, finished detection, wait/send_keys in plain words, every way it stops, no blocklist, the risk of text in output steering the AI, passwords). CHANGELOG entry. Spec: "As built in stage 3" note (25-command limit ends the turn with a note instead of a question).
- [ ] Commit `AI autopilot: README, changelog and spec notes`.
