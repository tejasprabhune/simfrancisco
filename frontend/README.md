# SimFrancisco — simpler map frontend

A clean, zero-build web app that visualizes SF-wide poll predictions on a map of
San Francisco. Type a question, and the synthetic electorate's verdict
accumulates across the city as green (yes) / red (no) dots.

This is the **simpler** of the two frontends: a real SF outline (no pixel art,
no zoom), with stochastic per-dot poll results. It talks directly to the
deployed backend (`https://sf-digital-twin-tp.fly.dev`) — see `../INTEGRATION.md`.

## Run

No build, no install. Serve the folder over HTTP (ES modules need `http://`, not
`file://`):

```bash
cd frontend
python3 -m http.server 5173
# then open http://localhost:5173
```

or `npx serve -l 5173` / any static server.

## Use

- The map fills the page and shows ~1,200 real synthetic residents as dots.
- Click the **✦ button** (top-left), or press **⌘K** / **/**, to open the
  Spotlight. Type a question (e.g. *"Do you support more public transit
  funding?"*) and hit **↵**.
- The query summary + a live response progress bar appear top-right; green/red
  verdicts accumulate across the map into a poll-map distribution.
- When complete, the Spotlight returns with the result distribution. **Dismiss**
  to clear, or **Ask another** to run a new prediction. **Esc** closes.

## How it maps to the backend

| UI step | Backend call |
|---|---|
| boot | `POST /simulations` → `GET /branches/{main}/agents` (real lon/lat for dots) |
| submit a prediction | `POST /simulations/{id}/branches` ("triggers the branching") |
| accumulating results | `POST /branches/{id}/poll` → `p_yes` + CI + breakdowns |
| dismiss | `DELETE /branches/{id}` (cleanup) |

**Note on the per-dot colors:** `/poll` returns an *aggregate* (`p_yes`), not a
verdict per agent. Per the simpler-frontend spec, the per-dot green/red is a
**stochastic visualization** — `src/verdict.js` assigns colors so the on-screen
yes-share matches `p_yes` exactly while forming organic regions (a real poll-map
look). The top-right "summary" currently shows the question verbatim; if the
backend adds a summarize endpoint, it slots into `runPrediction()` in
`src/app.js`.

## Files

```
index.html          page shell + UI overlay
styles.css          Fog & Bay palette, frosted-glass chrome
src/config.js       backend URL, sim params, palette, timings
src/api.js          backend client (fetch + timeouts)
src/projection.js   lon/lat → screen + point-in-polygon
src/map.js          canvas renderer (landmass, dots, reveal animation)
src/verdict.js      stochastic per-dot yes/no (marginal == p_yes)
src/sf-outline.js   embedded SF land outline (GeoJSON)
src/app.js          state machine + UI orchestration
```
