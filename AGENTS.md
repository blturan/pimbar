# Working on pimbar

## Proposing a design change

Whenever you propose a change to how the panel looks, show it as an HTML mockup
before writing any code. Don't describe it in prose or ASCII art.

- Write one self-contained HTML file to `/tmp/pimbar-<topic>-mockup.html` (outside
  the repository) and `open` it.
- Link pimbar's own stylesheets by absolute `file://` path (`ui/style.css` and
  `ui/rounded.css`), and build the mockup from the classes the panel already uses
  (`.panel`, `.section`, `.title`, `.card`, `.row`, `.name`, `.sub`, `.pill`,
  `.btn`, …). That way it looks like the real thing.
- Draw each option twice, side by side: light (`color-scheme: light`) and dark
  (`color-scheme: dark`) panels, 400px wide like the real one.
- Show every state the design has (e.g. idle, busy, success, error, empty), each
  labelled.
- Include what's there now as the first option, for comparison. Then give 2–3
  options, each with a title and a sentence or two on its trade-offs. Mark the
  one you'd pick as recommended.
- Wait for the user to choose before building it.
