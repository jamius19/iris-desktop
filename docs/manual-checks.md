# Manual checks

UI, overlay, paste and keychain behaviour are checked by hand. Tick per platform: Windows, Linux X11, Linux Wayland.

## M1 – Shell, design system, settings pages
- [ ] Compare Home and Transcription Provider with the Figma light screens (fileKey `AJazbOeWLUADj6qsSuxdfJ`) side by
      side.
- [ ] Dark mode: switch the OS theme while the window is open; all pages update.
- [ ] Every dropdown opens below, or above near the window bottom; the arrow turns; keyboard Up/Down/Enter/Esc work.
- [ ] Text fields commit on blur and Enter (Ctrl+Enter in the prompt and JSON areas), show errors, Esc reverts, trash
      clears; API key field shows dots after saving, eye reveals typed input only, trash deletes from Credential
      Manager / Seahorse.
- [ ] Sliders: drag, click, arrow keys, value saved on release; the mouse wheel scrolls the page, not the slider.
- [ ] Slide transitions in and out on every page; Custom Words returns to Post Processing; Esc, Alt+Left and the mouse
      back button go back.
- [ ] Tab moves focus with a visible ring; hover overlays on cards, rows and buttons.
- [ ] About shows the AboutSlint credit and opens every licence accordion (monospace text).
- [ ] Wayland without wl-clipboard/ydotool: Text Input row shows Missing with the right helper.
- [ ] Post Processing: "+" opens a draft named "Unnamed Provider" with the name field focused; Save checks name, https
      base URL and key; a saved provider gets its favicon; changing the host asks for a new key; Delete Provider
      removes it and its key and selects Vercel AI Gateway.
- [ ] Prompts: "+" adds "New prompt" (then "New prompt 2"), selects it and focuses the name; trash is disabled with one
      prompt left.
- [ ] Custom Words: add (Enter or "+"), over 60 characters and over 100 entries show the errors, delete works.
- [ ] Every setting survives a restart; no API key appears in `settings.json`.
- [ ] Release build on Windows opens without a console window.
- [ ] Without GL (a VM or an RDP session without GL), the app relaunches once with the software renderer and the log
      shows the femtovg error; with `SLINT_BACKEND` set, a failure exits instead of relaunching.

Checked by the implementer on 16/11/2026 (Windows 11, locked desktop, software renderer via `SLINT_BACKEND`,
window captured with `PrintWindow` and driven with posted mouse/keyboard messages): Home, Transcription (On device and
Deepgram), the provider dropdown, Post Processing (incl. a new draft), Custom Words, Appearance, Dictation and About
render in dark mode; navigation and Esc back work; a dropdown pick, a typed Language Code (commit on blur), a slider
click and an API key save + trash delete all worked; values persisted across a restart. Not seen: femtovg output,
light mode, the arrow rotation (the software renderer doesn't rotate images), hover/focus rings, Linux window
contents (WSLg window opened on Wayland and X11 but its contents can't be captured).

## M2 – Local dictation
Not checked by the implementer unless noted under "Checked" below: the Windows desktop was locked (no focus, click,
microphone, paste or screen checks), and there was no real Linux desktop (only WSLg: Weston, no layer-shell, no
`/dev/input`, no wl-clipboard/ydotool/xdotool).
- [ ] Download, cancel, resume, verify and delete each of the four models from the Transcription page; the progress
      line, "Deleting…", "Downloaded" + size, and "Another model is downloading." on another model show; the model
      menu marks downloaded models.
- [ ] Each model transcribes a 30 s English sample in a dictation; Whisper with a set language; Parakeet TDT v3 in
      German.
- [ ] Hold or Toggle: hold > 300 ms then release stops; a quick tap keeps recording until the next tap. Toggle and
      Push to Talk modes. Escape cancels only while listening and is free otherwise (type Esc in another app).
- [ ] Shortcut field: click → "Press keys…", a new combination saves and works at once; Esc cancels without leaving
      the page; the trash resets; the Global Shortcut row shows Registered / Not registered with the helper.
- [ ] Overlay never takes focus (type in an editor while it shows; click ✕ — the editor keeps the caret); top and
      bottom positions; on the monitor with the cursor (Windows, X11); at 100 % / 150 % / 200 % scaling; two monitors
      with different scales.
- [ ] Overlay states: dot + waveform (moves with the voice) + timer while listening; the live panel grows with the
      Lines setting and Max Width; spinner while finalizing; ✓ for 1.5 s; errors and notices for 3 s; clicking a
      notice dismisses it; "Loading model..." when the first load takes over 2 s; transparent corners without a dark
      fringe (femtovg).
- [ ] Live text streams in the panel for Nemotron and Parakeet Unified; per-phrase models show text phrase by phrase
      (Skip Silence on).
- [ ] Paste into Notepad, VS Code, a browser field and a terminal (Ctrl+Shift+V) on each platform; clipboard text and
      an image are restored (Windows, X11); Direct typing on Windows; non-ASCII text ("Grüße, naïve café") via Ctrl+V
      on Wayland; Add Trailing Space; Paste Delay / Restore Delay.
- [ ] Wayland without ydotoold: the notice "Couldn't paste. The text is on the clipboard." shows and the text is on
      the clipboard.
- [ ] Wayland: wl-copy gets the text on stdin; non-ASCII text ("Grüße, naïve café, 東京") pastes intact into a GTK
      app (gedit), a Qt app (Kate) and an XWayland app (e.g. `xterm`).
- [ ] Wayland: an image (or other non-text) on the clipboard is still there after a dictation; with the clipboard
      empty, it is empty again afterwards.
- [ ] Direct typing on Linux: `ydotool type --file -` with ydotool 0.1.8 and with 1.0.x (Wayland), and `xdotool type
      --file -` (X11), including non-ASCII text; a long transcript isn't cut off by the typing deadline.
- [ ] A hung clipboard owner (e.g. `wl-copy --foreground` stopped with `kill -STOP`, or a frozen app that owns the
      clipboard): the paste gives up after about 2 s per tool and the dictation ends with a notice, not a hang.
- [ ] The udev rule: after installing it, `getfacl /dev/input/event*` shows the user only on keyboards (not mice or
      touchpads) and on `/dev/uinput`; ydotoold started as the user works.
- [ ] Single instance on Linux: `$XDG_RUNTIME_DIR/iris-desktop.sock` exists while Iris runs; a second `iris`
      forwards; after `kill -9` the next start replaces the stale socket. Without `XDG_RUNTIME_DIR` the abstract name
      is used.
- [ ] `iris --toggle-transcription` and `iris --cancel` from a terminal / compositor binding act on the running app;
      a second `iris` brings the window to the front; `pkill -USR2 -n iris` toggles on Linux; with Iris not running,
      `iris --toggle-transcription` just starts it.
- [ ] Worker crash: kill the `iris --transcribe-worker` process mid-dictation → an error notice, no app crash; the
      next dictation works.
- [ ] Unload Model: Immediately, 2 min and Never behave as labelled (watch the worker process appear and exit).
- [ ] Accelerator CPU only vs Auto on a Vulkan GPU (log shows the device); Auto on a machine without Vulkan falls
      back silently.
- [ ] Microphone: pick a specific device; unplug it mid-dictation → "Couldn't open the microphone"; Windows privacy
      switch off → the Windows message.
- [ ] Linux without the udev rule (AppImage): Global Shortcut row → Setup installs it via pkexec and the row turns
      Registered without a restart.
- [ ] Linux helper: layer-shell mode on KDE and Sway/Hyprland (no focus, clicks reach ✕), X11 mode on GNOME
      (XWayland) and on an X11 session.
- [ ] Skip Silence on/off and Speed 1.5× with a per-phrase model; silence only → "No speech detected".
- [ ] The `#[ignore]` engine and VAD tests, on Windows and Linux, after `cargo build -p iris` (a debug worker: fault
      injection is compiled out of release): `IRIS_TRANSCRIBE_WORKER_EXE=target/debug/iris IRIS_TEST_MODEL=<streaming
      gguf> IRIS_TEST_WAV=<16 kHz mono wav> cargo test -p iris -- --ignored --nocapture --test-threads=1` (the
      supervisor tests and `local::vad`'s scorer test); all pass.

Checked by the implementer on 16/11/2026: `iris -f` through the real worker on Windows (Vulkan, RTX 3050): Parakeet
TDT v3 and Nemotron Streaming English both returned the exact sentence, also on a WAV cut right after the last word
(no tail padding needed). See the M2 report for WSL results.

## M3 – Cloud speech-to-text and LLM cleanup
Nothing here was checked by the implementer: there are no API keys on the development machine and the desktop was
locked. The providers pass their ported tests against local mock servers only; the two public catalogues (Vercel AI
Gateway, OpenRouter) were fetched once and parse. Each item needs a real key for the provider.
- [ ] OpenAI: `gpt-live-transcribe` (live text), `gpt-4o-mini-transcribe` (Realtime, text after stop) and an upload
      model (an id from the model menu that isn't in the Realtime list); language set and blank; Custom Words on
      (`keywords` for gpt-(live-)transcribe, a prompt for whisper-1 / gpt-4o(-mini)-transcribe).
- [ ] Vercel AI Gateway: a live model (`openai/gpt-live-transcribe`) and a non-live one (`openai/whisper-1`); the
      model menu marks live models; picking one stores live or upload (live text only for live ones).
- [ ] Deepgram: `nova-3` live and with "Disable Realtime Streaming"; Custom Words (keyterms) on with Nova-3; language
      blank (multi) and `de`.
- [ ] Soniox: `stt-rt-v6` live and an async model; a US key in the US region, an EU key in the EU region (and the
      error with a US key in EU); Language Codes "en, de"; the model menu loads after the key is saved and after a
      region change.
- [ ] Speed 1.5× and 2.0× on a live and an upload model per provider (not Soniox real-time: hidden there).
- [ ] Skip Silence: a 10 s pause is dropped (log `VAD: sent X ms of Y ms`); silence only → "No speech detected"; a
      pause over 30 s with a live model keeps the socket (keep-alive zeros, pings).
- [ ] Cleanup with Vercel AI Gateway, OpenRouter and a custom provider (Send JSON Schema on and off; a model that
      thinks out loud has its `<think>` block stripped); Reasoning Level options follow the model (an unlisted saved
      level shows "Not listed for this model"); Provider Options are sent.
- [ ] Cleanup Timeout 1 s with a slow model: "Cleanup timeout. Pasted original text." and the original is pasted.
- [ ] Wrong key, no credit / quota, unknown model: each provider's message shows in the overlay (speech-to-text) or
      as the cleanup notice with "Inserted the original text"; no key → "Add your … API key in Iris settings".
- [ ] Model menus: the refresh button spins (femtovg) while loading and reloads; the first open of an unloaded menu
      loads it; load errors show under the field; OpenAI's list reloads after its key changes; a custom provider's
      list reloads after Save.
- [ ] Keychain locked or unavailable (Linux without a Secret Service): the API key fields (including a saved custom
      cleanup provider's) say "Couldn't read the keychain." and a dictation with a cloud provider shows the same.
- [ ] Filler words: "um, so, uh, I think" with Language blank (English detected) and set to `de` ("äh").
- [ ] On device: after picking another downloaded model, the worker loads it within seconds without a dictation
      (warm-up); cancelling (Esc) during a per-phrase decode stops the worker and the next dictation reloads it.
- [ ] Network drop mid-dictation (disable Wi-Fi): a readable "Network error: …" (Gateway: one reconnect first),
      never a hang past the timeouts.
