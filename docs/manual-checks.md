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
