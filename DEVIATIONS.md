# Deliberate differences from the Python Studio

The Python FLE Studio is the specification of `fle-api`, and the tests compare both request by request. Where Python has a
mistake, this server does **not** copy it: the difference is listed here, and the test knows about it (it converts the Python
text or skips the Python call, and asserts the intended behaviour on the Rust side instead).

| Id | Python | Here | Why |
|---|---|---|---|
| D2 | A start next to an attached server is allowed and runs a second engine | 409 "A server is attached. Disconnect it on the Engine page first." | Two engines at once eat the RAM and GPU memory of one |
| D3 | Removing an engine whose data folder is behind a symlink does not delete the folder (resolved and unresolved paths are compared) | Both sides resolved: the folder is deleted | Plain bug; the tests run without symlinks in the path so the two agree |
| D5 | Unported routes answer 404 | 501 "<feature> is not in this version of FLE yet." | A clear message, not something that looks like a bug |
| D7 | `/api/chat/capabilities` `pdf` is true when `pypdf` is installed | always false | no PDF text extraction yet |
| D8 | The page's `system.python` is the Python version | empty string | this server is not Python |
| D9 | A pure-Python crash for `{"results": 5}` from a search server | an empty result | robustness |
| D10 | The page is the FLE Studio page | FLE Link's own page: its name, cards of measurements a server does not have hidden (a 501 hides the card), a second try without tools when a model writes an unreadable tool call. The tests check that the binary serves exactly `web/studio`; `/api/state` is still compared with Python | FLE Link is its own product; the page was a frozen copy until 0.1.0 |
| D12 | GPU `auto` sets `CUDA_VISIBLE_DEVICES` to the GPU with the most memory, also when the variable was already set | A `CUDA_VISIBLE_DEVICES` the program was started with is kept, and `auto` chooses among those GPUs | On a shared machine the Studio put its engine on a GPU that belonged to another service |
| D13 | GPU `auto` takes the GPU with the most total memory | the GPU with the most free memory | A large card that another service fills is the worst place for a new engine |
| D14 | The tuner screens threads, batch and `--n-cpu-moe` with one `llama-bench` call before the trials (llama.cpp only) | No screening: every value gets a full trial | `llama-bench` is not part of what FLE Link installs; the trials decide anyway, the screening only saved time |
| D15 | Forge: "Use the Python library as a first trial" mixes a calibration text from the Python standard library | No such button; `stdlib` answers 400 | FLE Link carries no Python; the bundled text (85/15 split, CC0/MIT) is the start. The Forge is otherwise compared job by job with the Python plugin (`tests/forge_conformance.rs`: same files, same SHA-256) |
| D16 | A download whose connection breaks stops with "run again to resume" | The terminal's download picks it up again by itself where it stopped (up to five tries in a row without progress, pauses 2–16 s), and a connection silent for 60 s counts as broken; a machine without network still fails at once | 248 GB do not survive one try; the Studio's download worker is unchanged |
| D11 | `doctor` keeps `auto` on a machine without NVIDIA GPU or with an upstream engine and reports the FLE checks as failures | `auto` becomes `off` there, like `flelink run` | `doctor` failed on every Mac and with every plain llama.cpp |

Gone from the list because Python fixed them itself (0.4.1): D1, a plan error with a hint was printed as a tuple; now the two
sentences are joined, as here. D6 (the update panel) and the hosts list are plugins of the Python Studio now; the core, which is
what this server replaces, has neither.

**Which Python.** The specification is the *core* of FLE Studio `main` at the commit in `STUDIO_PIN`, started with
`FLE_STUDIO_NO_BUNDLED=1` (no plugins: Forge, Updates, Remote access, Recorder and the model switcher are Python plugins there).
Moving the pin is a deliberate act: change the file, run the comparison tests, port what differs.

Things that stay the same on purpose even though they look odd (and are candidates for a later, announced change): a start without a
model name uses the last model; chat lists with equal times keep the file system's order; a page is sent with `Cache-Control` twice.

## D10: zone abbreviation of the local clock

On a machine whose zone is the IANA name `GMT`, the clock tool says `GMT` where Python says `UTC` (Rust takes the abbreviation
from the IANA zone, Python from the C library). Same zone, same offset; the test treats the two spellings as equal.

## D11: the program is called `flelink`

Hints, error texts and the saved config header say `flelink ...` where the Python helper says `fle ...`. The comparison tests
apply one rule to Python's output (`common::brand`) and compare everything else word for word. The same for the notes FLE Link writes into the
engine log: `FLE Link: …` where the Python Studio writes `FLE Studio: …` (the engine test swaps the name before comparing).

## D12: installed engines before the PATH

`find_engine` looks at the explicit path, then `FLE_ENGINE`, then the engines FLE Link installed itself, and only then at a
`llama-server` on the PATH. The Python helper looks at the PATH before its own installs, so a Homebrew llama.cpp would win
over the engine the program chose and checked.
