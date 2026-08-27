# GitHub support — research notes

How T3 Code implements GitHub, and how it maps onto Waku's daemon/GPUI split.
Source: shallow clone of `pingdotgg/t3code` at `/Users/jd/Development/personal/t3code`
(main, 2026-08-27). All paths below are relative to that clone unless prefixed with `waku:`.

## Verdict

Doable in Waku with a bounded scope. The heavy lifting Waku already has: a daemon
that owns subprocess work, WS RPC/event transport, virtualized transcript lists,
cached-background-fetch patterns with generation guards, and a settings surface.
`gh` gives us auth, REST, and GraphQL for free. Phase 1 (read-only PR list +
detail) is the lazy first slice.

## T3 Code architecture

**Everything goes through the `gh` CLI subprocess. No octokit, no direct
api.github.com HTTP.** The one provider using direct HTTP is Bitbucket
(`apps/server/src/sourceControl/BitbucketApi.ts`), GitHub deliberately does not.

- Server-owned: all GitHub code lives in `apps/server/src/`; the web/desktop
  clients speak WebSocket JSON-RPC only and make zero GitHub calls.
- Provider-port pattern: `sourceControl/GitHubSourceControlProvider.ts` (kind
  `"github"`) behind `SourceControlProviderRegistry`, alongside GitLab/Azure/Bitbucket.
- CLI wrapper: `sourceControl/GitHubCli.ts` — Effect service spawning `gh` with a
  typed error union (unavailable / auth / rate-limit / decode).
- PR feature: `pullRequest/GitHubPullRequestCli.ts` (~1.7k lines) — every read and
  write. `pullRequest/gitHubPullRequestJson.ts` — GraphQL documents + effect-Schema
  decoders for `gh api` output.

### Read path (PRs only — no issue-tracker support at all)

- Lists: `gh pr list --state --limit --json <fields>` (`GitHubPullRequestCli.ts`
  `listPullRequests`); search + stats via GraphQL (`gh api graphql --input -`,
  query on stdin — "argv is visible in process listings").
- Detail: `gh pr view --json`; diff via `gh pr diff`; review threads/comments and
  reactions via GraphQL (unreachable through `gh pr view`).
- Shapes in `packages/contracts/src/pullRequest.ts`: `PullRequestListEntry`
  (title/number/state/draft/mergeability/labels/checksState/reviewDecision),
  `PullRequestDetail` (+`viewerPermissions`, `capabilities`), `PullRequestActivity`
  (comments, reviewThreads, commits).
- Client cache: SWR atoms (list stale 30s, detail/activity 15s, stats 60s) +
  `useLiveRefresh` (5-min poll while visible, stops after 6 min idle) + manual
  `pullRequests.invalidate` RPC. Server side: effect `Cache` with
  stale-while-revalidate; mutations bump epochs to invalidate.
- Rate limiting: per-host lease service with exponential backoff 30s→15min
  (`sourceControl/SourceControlRateLimit.ts`), plus a GraphQL quota budget that
  reserves 10% of the API quota and reads `rateLimit {cost remaining}` from every
  response (`sourceControl/githubGraphQlBudget.ts`).

### Write path

- PR actions through `gh`: merge (incl. auto-merge + method), ready/draft,
  close/reopen, update-branch, comment; review submission and reviewer
  request/dismiss through `gh api --method POST/DELETE` REST; thread
  reply/resolve, reactions, title/body edits through GraphQL mutations
  (`GitHubPullRequestCli.ts` L806–1852).
- **No issue mutations anywhere** — t3 has no issue-tracker surface.
- Safety: fresh `viewerPermissions` GraphQL read before every write, server-side
  capability re-check per mutation, confirm dialogs for merge/close, optimistic
  reactions with rollback, per-action in-flight state.
- **The coding agent gets no GitHub tools**: no GitHub MCP server, no `gh` prompt
  injection. An agent can only touch GitHub by shelling out itself, gated by the
  normal runtime approval modes.

### Auth

- **None of their own.** Probe = `gh auth status --json hosts` (gh ≥ 2.81);
  parse accounts, prefer active authenticated (multi-account read-only).
- No token storage, no OAuth/PAT flow; they even strip `token:` lines from gh
  output. UI is just an auth badge + "Rescan" (`SourceControlSettings.tsx`).

## Mapping onto Waku (GPUI standards)

1. **Daemon owns all GitHub I/O** — mirrors T3's "server owns the calls" split and
   our own rule that `gh` (a subprocess, 100–500ms) must never be reachable from a
   frame. Fetches land via `cx.background_executor().spawn`, results stored on the
   entity, `cx.notify()` on arrival, generation counter so a superseded fetch
   cannot overwrite newer state (same pattern as the checkpoint-ref cache).
2. **Virtualized lists** — issue/PR lists use `list()`; the daemon resolves a
   collection in one background pass and row builders read only in-memory stores
   (sidebar-branch-cache discipline: no I/O per visible row per frame).
3. **Cadences** — no unthrottled polling: bundle refreshes, respect the stream
   commit cadence (≤ ~8.3 Hz) and route animated refresh indicators through the
   pulse-lease pattern (`src/ui/motion.rs`), honoring `cx.reduce_motion()`. Read
   `docs/performance.md` before touching anything the render path reaches.
4. **Accessibility** — list navigation via `tab_group` + arrow keys, enter opens,
   escape closes, visible focus; state shown as glyph + text (never color alone);
   action targets keep generous hit areas.
5. **Widget references** — build from Zed's crates (lists, popovers, menus), not
   `gpui-component`. T3's right-panel detail (Summary/Timeline/Code tabs beside the
   transcript) informs the product shape, not the implementation.

## Proposed scope for Waku

- **Phase 1 — read**: `waku-daemon` GitHub module (`gh` wrapper + typed errors +
  auth probe via `gh auth status --json` + per-host rate-limit pause), WS RPC
  methods (`github.listPullRequests`, `github.pullRequestDetail`, …), client state
  stores, a sidebar entry with virtualized list, right-panel detail (summary +
  timeline + diff).
- **Phase 2 — write**: comment / merge / close / ready via the same daemon
  module, with permission-gated controls and confirm dialogs.
- **Phase 3 — differentiator**: issues (t3 has none), and optionally exposing a
  curated GitHub tool to agent sessions (Waku already has a subagent/tool
  pattern) rather than raw `gh` shell access.
- **Out of scope until needed**: OAuth/device flow (gh owns auth), webhook push
  (polling + invalidation first), GitLab/Azure/Bitbucket providers.

## Risks

- `gh` missing/unauthenticated → modeled as first-class "unavailable" state (T3's
  `GitHubCliUnavailableError` / setup-copy pattern), surfaced in settings + panel.
- Rate limits → daemon-side pause + retryAt surfaced to the UI; keep a GraphQL
  budget if we lean on search endpoints.
- Long diffs → cursor-paged fetches with byte caps (T3 caps `gh pr diff` and pages
  100 files), never one giant spawn result through the UI layer.
