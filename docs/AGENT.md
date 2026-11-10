# Agent

The Agent page runs **[FLE Durable](https://github.com/sirfyyn/fle-durable)** next to your model: a local agent that is always on and works from events instead of
waiting for you to type. It uses the model on the Engine page through FLE Link's own `/v1`, so it follows whatever model
you run.

## What it does

- **Agenda.** You plan events on the page: once (in N minutes or at a time), every day, on certain days, or every N minutes.
  When one is due, the agent gets it as a message and acts on it. The agent plans its own follow-ups the same way
  (`agenda_plan`: "check in 20 minutes whether the download finished"); the page marks those as *planned by the agent*. The
  agent can cancel only its own items, never yours, and never gives its items right of way.
- **Webhooks.** Any tool can hand the agent an event: `POST /api/p/agent/events` with the agent's key in `X-Agent-Key`
  (shown on the page) and `{"text": …, "id": …, "von": …}`. The same `id` twice counts once. From another computer add the
  access key as Bearer token. Events from webhooks never get right of way.
- **One durable queue.** Every event lands in a queue in a SQLite file before the agent sees it. A crash or restart neither
  loses an event nor runs one twice. Your own events (with right of way) go before background work.
- **Approvals.** Only destructive steps (deleting, overwriting, anything irreversible) wait for you; they appear on the page
  with a short context and the exact command.
- **Its own chat.** "Open its chat" opens FLE Durable's chat page with a one-time code; its token stays inside FLE Link.

## Connect

FLE Durable is its own program; FLE Link only talks to it. On the Agent page either enter the **address and token** of a FLE Durable that
already runs (on this computer or another one), or follow the steps below and let FLE Link start it.

While it answers, the chat in FLE Link is your agent, with a switch to **Direct to model** for the plain chat. A saved chat always opens directly.

FLE Link uses these routes of its interface: `/api/health`, `/api/message`, `/api/stream`, `/api/conversation`, `/api/approvals`, `/api/abort`,
`/api/agenda`, `/api/queue`, with the token as a Bearer header.

## Set up

1. Node.js 22.19 or newer, and git.
2. Agent page → **Install FLE Durable**. FLE Link fetches the FLE Durable version it was tested with (`DURABLE_PIN`, like
   the engine's `ENGINE_PIN`) into `<data>/agent/app` and installs its packages; nothing to clone by hand. A newer FLE Link
   that pins a newer FLE Durable shows **Update FLE Durable** while the agent is off. The new copy is built next to the old
   one and only replaces it once it worked.
3. Start the engine, then the agent. "Start with FLE Link" starts it with FLE Link from then on.

Or in a terminal: `flelink code --setup` does the same install.

## Always on

The **Always on** switch on the Agent page, or `flelink service on`, makes FLE Link start at login with the last model and the agent; the agent waits until the model is loaded. It comes back if it stops. On Linux a user service ends with the last login unless the system keeps it: `loginctl enable-linger <user>`. `flelink service off` undoes it, `flelink service status` tells.

## Tickets, calendar, events

The card at the bottom of the Agent page shows what you and the agent keep track of: tickets with their status, the calendar for the next four weeks, and the latest events that came in. FLE Link passes only these routes to the agent. A remote agent reached with a limited token answers 403 and the card stays hidden.

## Accounts

Settings > Accounts connects Hugging Face, GitHub and Google with a click; the sign-in opens on the provider's own page and must happen on the computer that runs FLE Link for Hugging Face and Google. With Google connected, FLE Link reads your main calendar every five minutes, from a week back to 90 days ahead, and the agent sees it as a mirror. It goes one way for now: changes there show up here, FLE Link writes nothing to Google. A calendar that cannot be read is named as such and keeps its last entries. iCloud works the same way over CalDAV, for all of your calendars; Apple wants an app-specific password for it (account.apple.com, Sign-In and Security), kept in a file only you can read. Disconnecting removes the mirror from the agent, also after a restart.

## Computer use

Off unless you allow it on the Agent page, 30 minutes at a time. It is off again at every start of FLE Link, only this computer can allow it, and until then every screen, mouse and key call of the agent is refused. It needs on macOS the Screen Recording and Accessibility permissions, on X11 `xdotool` and ImageMagick (`wmctrl` for window lists), on Wayland with a wlroots compositor `grim`, `wtype` and `ydotool`. Windows is not supported yet. An agent that may run shell commands acts as your user anyway: this switch governs its screen, mouse and keyboard tools, it is not a sandbox.

## In a terminal: `flelink code`

In a project folder, `flelink code <recipe>` (or `flelink code` while a model is running) starts FLE Durable for that folder and shows its answers and tools in the
terminal. It installs FLE Durable first if it is missing. `AGENTS.md` or `CLAUDE.md` in the folder become its instructions, and each folder keeps its own conversation in `<data>/code/`.

`/direct` talks to the model itself, `/agent` goes back, `/quit` leaves, Ctrl+C stops a turn. The agent reads, writes and runs commands in the folder without asking;
it asks only before steps that cannot be undone.

Working on FLE Durable itself? Under "Own FLE Durable folder" enter your checkout
(`git clone https://github.com/sirfyyn/fle-durable && cd fle-durable && npm ci --ignore-scripts`); FLE Link then starts that one.

## Where things are

`<data>/agent/` in FLE Link's data folder:

| File | What |
|---|---|
| `app/` | the FLE Durable FLE Link installed (`.flelink-pin` names its commit) |
| `config.json` | FLE Durable's config. FLE Link sets the model window, the time zone and the device on every start and keeps everything else, so keys you add stay (for example `ports.sources` for event sources of your own). |
| `durable.sqlite` | conversations, agenda, queue, approvals |
| `work/` | the agent's working folder (setting `workspace` moves it) |
| `settings.json` | the page's settings: `runner_dir`, `node`, `autostart`, `language` (`en`, `de`), `workspace`; apart from FLE Link's own settings |

## Routes

The page talks to the runner only through FLE Link; the runner listens on the loopback address with its own token.

| Route | What |
|---|---|
| `GET /api/p/agent/status` | installed, Node, running, port, the last log lines |
| `POST /api/p/agent/start` · `stop` · `restart` | start with the model the engine runs now |
| `POST /api/p/agent/setup` | `runner_dir`, `node`, `language`, `workspace`, `autostart` |
| `GET` · `POST /api/p/agent/agenda` | list, plan (`kind`, `text`, `at` / `in_minutes` / `time` / `days` / `every_min`) |
| `DELETE /api/p/agent/agenda/<id>` · `POST …/<id>/pause` | remove, pause or resume |
| `POST /api/p/agent/events` | webhook |
| `GET /api/p/agent/queue` | what waits for the agent |
| `GET /api/p/agent/approvals` · `POST …/<id>` | open approvals, decide (`decision`: `granted` or `refused`, `note`) |
| `GET /api/p/agent/conversation` · `POST /api/p/agent/message` | the conversation; write to the agent |
| `POST /api/p/agent/chat` | a one-time address for its chat page |
