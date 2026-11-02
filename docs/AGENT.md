# Agent

The Agent page runs **FLE Durable** next to your model: a local agent that is always on and works from events instead of
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

## Set up

1. Node.js 22.19 or newer.
2. FLE Durable in a folder, its packages installed once: `npm ci --ignore-scripts` in that folder.
3. Agent page → Set up: the folder (and Node's path if it is not on the PATH). Start the engine, then the agent.
   "Start with FLE Link" starts it with FLE Link from then on.

## Where things are

`<data>/agent/` in FLE Link's data folder:

| File | What |
|---|---|
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
| `GET /api/p/agent/approvals` · `POST …/<id>` | open approvals, decide (`entscheidung`: `erteilt` or `abgelehnt`) |
| `POST /api/p/agent/chat` | a one-time address for its chat page |
