### F20 · Agent browser

Each implementation thread receives `velocity-browser` when its ACP session starts or resumes.
The browser launches on the first tool call and uses the installed Chromium engine, Microsoft Edge on Windows and Chrome or Chromium elsewhere.
It runs headless and receives input through the Chrome DevTools Protocol.
The engine uses a separate profile in the project's workspace location on a non-system drive on Windows.
The thread retains that profile location when workspace settings change.

`open` creates a tab and `navigate` changes the current tab, including HTTP and HTTPS localhost dev servers.
`back` and `reload` operate on the current tab, and `tabs` lists, switches or closes tabs.
`snapshot` returns a compact accessibility tree with element refs.
`click`, `type`, `press`, `select` and `scroll` use refs from the latest snapshot.
`wait_for` accepts text, a CSS selector or network idle.
`screenshot` saves a downscaled JPEG and returns its path, with image content available on request.
`console` and `network_errors` drain their bounded logs, and `evaluate` returns a bounded result from a read-only JavaScript expression.

The Browser tab and the board card's Browser button open the live view with the URL and tabs.
Take over pauses agent browser calls and enables owner input inside the view.
Hand back resumes browser calls, invalidates element refs and tells the agent the active page and tab count.
Leaving the view also hands control back.
Agents ask the owner to take over for sign-in and never enter passwords, API keys or tokens.
Velocity displays takeover frames in memory and disables the browser profile's password manager and form autofill.

Settings → Agents defaults to three concurrent browsers.
Each browser process tree has a 1 GiB memory budget and appears in Usage.
New browsers wait when the machine has insufficient headroom for that budget.
A browser closes after ten idle minutes and the next tool call reopens the same profile.
Ending the agent session closes its process tree, exiting Velocity ends its owned processes, and deleting the thread removes its profile.
Velocity uses a job object on Windows and a process group elsewhere.
