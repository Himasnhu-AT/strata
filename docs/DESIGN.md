# Strata design

Mockups: the "Strata — log viewer" design canvas (artboards Main, Agent, Palette, Open).

## Visual language (shared with the other ashlar showcase apps)

- Dark first; a light theme follows the OS setting.
- Fonts: Inter for the interface, JetBrains Mono for log text and data.
- Colors: background #0F1115, panels #161A20, raised #1D222A, borders #2A303A, text #E6E8EB, muted
  #9AA3AE. Accent amber #F2A93B. Levels: error #F2555A, warn #FFD36E, info #5B9DFF, debug muted,
  ok #3CC8A0. Level is never shown by color alone: each row also has the level word.
- 13 px interface text, 12 px mono rows, 28 px rows, 8 px spacing grid, 6 px radius.

## Window layout

```
┌ title bar 38 ─────────────────────────────────────────────────────────────┐
├ toolbar 44: search · regex/case · level chips · filter chips · follow ────┤
├ sidebar 240 ┬ histogram strip 56 ───────────────────────────┬ agent 360 ──┤
│ Files       │ log table (virtual): # · time · level · svc · message │ (opt.) │
│ Bookmarks   │                                               │        │
│ Saved       ├ detail pane (resizable, 220 default) ─────────┤        │
├─────────────┴───────────────────────────────────────────────┴────────┤
└ status bar 24: lines · size · format · filter count · follow · memory · built with ashlar ┘
```

- **Title bar:** the file name and its path, dimmed.
- **Toolbar:** a search field (Cmd+F) with regex, case and whole-word toggles and a match counter
  ("12 of 4,812"); level chips with counts (Error, Warn, Info, Debug) that toggle; the active
  filter chips (removable); Follow toggle; a time-range button.
- **Sidebar:** sections Files (open files, with size and a live dot when following), Bookmarks
  (color, name, line), Saved filters, Recent. Folds, one Tab stop, type-ahead (ashlar sidebar).
- **Histogram strip:** lines per bucket, errors stacked in red, warnings in yellow; the visible
  range drawn as a bracket; drag to select a time range; hover shows the bucket's time and counts.
- **Log table:** line number, time, level, service (or the first parsed field), message. Rows of
  error level get a faint red background as well as the word. Search matches are underlined in
  amber; agent highlights get an amber left mark and a small "AI" tag. Columns resize, reorder,
  hide (right-click the header).
- **Detail pane:** the selected line pretty-printed; JSON as a folding tree; buttons Copy line,
  Copy JSON, Filter by this value (on a field).
- **Status bar:** "1,284,113 lines · 1.9 GB · JSON lines · 2 filters · following · 74 MB · built
  with ashlar".

## Screens

1. **Open / empty:** a drop target, "Open a file" (Cmd+O), recent files, and a line on the agent:
   "Connect an AI agent in Settings ▸ AI". While indexing a big file: the table already shows the
   first lines; the status bar shows "Indexing 38 % · 1.9 of 5.0 GB · Esc to stop".
2. **Main:** as laid out above.
3. **Agent panel open:** the right panel with the agent's timeline, notes and Undo.
4. **Command palette:** Cmd+K, every command with its shortcut, fuzzy search.
5. **Settings:** General, Formats (custom formats later), Redaction patterns, AI (MCP on/off,
   install for Claude/Cursor, "Allow the agent to change the view").

## Keyboard map

| Keys | Action |
|---|---|
| Cmd+O / Cmd+W | open / close file |
| Cmd+F, Enter / Shift+Enter, F3 | search, next / previous match |
| Cmd+L | focus the filter bar |
| E / Shift+E | next / previous error |
| W / Shift+W | next / previous warning |
| Cmd+G | go to line or time |
| F | toggle follow |
| B / Cmd+B | add bookmark / show bookmarks |
| Space | toggle the detail pane |
| Cmd+K | command palette |
| Cmd+Z / Cmd+Shift+Z | undo / redo (filters, bookmarks, highlights, agent changes) |
| Cmd+Shift+A | toggle the agent panel |

## States

- **Loading:** first lines appear at once; indexing progress in the status bar, cancellable.
- **Search running:** the match counter counts up with a small spinner; results appear as found.
- **Nothing matches:** the table shows "No lines match these filters" with the filter chips and a
  "Clear filters" button.
- **File changed:** truncated or rotated while following: a banner "The file was replaced. Reload?"
- **Unreadable:** permission denied, not found, binary file: a dialog with the reason and the path.
- **Agent disconnected:** the panel keeps its history, the header dot turns grey.

## Agent panel behaviour

- Opens automatically on the first MCP call that changes the view; read-only calls only add rows.
- Rows: icon, verb, details, time. "Searched `timeout` · 312 matches", "Filtered service = payments",
  "Highlighted 3 lines", "Added bookmark 'first 502'".
- Clicking a row that changed the view scrolls to it and flashes the lines.
- "Undo agent changes" is one undo step; Cmd+Z after it brings them back.
- The switch "Allow changes" at the top; off greys out the view-changing rows' icon and makes those
  calls return an error.

## Accessibility

- Every control has an automation id and an AccessKit role (ashlar components do this).
- The table reports rows and cells to screen readers; the level is a word, not only a color.
- All actions reachable from the keyboard; focus is always visible.
- Contrast: text 4.5:1 or better on every surface; status colors checked against #161A20.
- Reduced motion: no animated scrolls or flashes; the histogram doesn't animate.
