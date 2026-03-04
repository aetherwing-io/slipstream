# Slipstream Brand Kit — Generation Prompt (v2)

> Feed this prompt to an LLM (Claude, etc.) to generate a single-file HTML brand kit for slipstream.
> The output should be a self-contained HTML file with all CSS inline, no external dependencies except Google Fonts.
>
> **v2 changelog:** Locked color direction (cyan/teal "Undertow"), locked mark (`s//`), locked domain
> (slipstream.aetherwing.io), specified CSS variable naming convention, added architecture diagram as
> styled element, tightened sentence-initial capitalization rule, added real voice sample.

---

## Prompt

You are a brand designer creating a comprehensive visual identity kit for **slipstream**, an open-source developer tool. The output must be a **single self-contained HTML file** with all styles in a `<style>` block. No JavaScript. No external assets except Google Fonts imports.

### What slipstream is

Slipstream is an **in-memory file editing daemon** for LLM agent workflows. It's written in Rust and runs as a persistent background process that preloads files into memory, accepts batch read/write/replace operations over a Unix domain socket, and flushes atomically to disk.

**The core value:** Instead of 10+ serial tool calls (each costing 1-3 seconds of LLM inference latency), an agent batches all file operations into 1-3 calls. It eliminates the round-trip tax.

**Key capabilities:**
- **One-shot mode:** Open files + read + edit + flush + close in a single tool call
- **Batch operations:** 50 mixed reads and writes in one RPC
- **Session-based concurrency:** Multiple named sessions (agents) can work on the same codebase simultaneously
- **Conflict detection:** Optimistic concurrency — non-overlapping edits merge cleanly, overlapping edits fail with actionable errors
- **Atomic flushes:** Disk files are either fully old or fully new, never partial (temp + rename)
- **4 MCP tools:** `slipstream`, `slipstream_session`, `slipstream_query`, `slipstream_help` — consolidated from 14 original tools via a verb-based DSL

**Performance story (real benchmarks):**
- Traditional approach: 18 tool calls → Slipstream: 1 tool call (94% fewer)
- 30-file batch edit: 4 tool calls total
- 50-operation mixed batch (reads + writes): verified correct in 1 batch call
- 50 rapid open/close cycles: zero leaked sessions, daemon stays responsive
- 3 concurrent CLI executions: zero corruption

**Architecture:**
```
┌─────────────┐  ┌─────────────┐  ┌─────────────┐
│  Agent A    │  │  Agent B    │  │  Agent C    │
└──────┬──────┘  └──────┬──────┘  └──────┬──────┘
       │                │                │
       └────────┬───────┴────────┬───────┘
                │  Unix Socket   │
                │  (JSON-RPC)    │
        ┌───────▼─────────────────▼────────┐
        │   Slipstream Daemon (Tokio)      │
        │  ┌─────────────────────────────┐ │
        │  │   Session Manager (DashMap) │ │
        │  ├─────────────────────────────┤ │
        │  │   Buffer Pool (Arc<RwLock>) │ │
        │  ├─────────────────────────────┤ │
        │  │   Flush Engine (atomic I/O) │ │
        │  └─────────────────────────────┘ │
        └──────────────────────────────────┘
```

**Render the architecture diagram as a styled CSS element** (not ASCII art). Agent boxes at the top, a dashed-border socket connector, and the daemon as a bordered container with internal layer rows. Use the slipstream accent color for the daemon border and socket label. This is a distinctive visual element mish's site doesn't have.

**Test results:** 240 unit tests passing. 28 stress tests: 26 PASS, 1 PARTIAL, 0 FAIL, 1 SKIP.

**Tech stack:** Rust, Tokio, DashMap, Unix domain sockets, JSON-RPC, MCP protocol.

**Part of the aetherwing-io organization** (github.com/aetherwing-io/slipstream) alongside mish (LLM-native shell), FCP servers (file context protocol), and other LLM agent tooling.

### Design direction — LOCKED DECISIONS

Slipstream is a **sibling product to mish** (ohitsmish.com). They share an organization but need **distinct identities**.

**Mish's identity (reference — do NOT copy):**
- Gradient: violet (#8b5cf6) → indigo (#6d5ce7) → blue (#4f8fff) — called "Spectral Shift"
- Icon: `μ|sh` in monospace with the gradient
- Fonts: Geist Mono (primary), Outfit (secondary sans)
- Dark backgrounds: void (#09090b), deep (#0f0f13), surface (#16161d)
- Personality: energetic, intent-aware, proxy/membrane metaphor

**Slipstream's identity (LOCKED):**

| Decision | Value | Rationale |
|----------|-------|-----------|
| **Color direction** | Cyan/teal | Water/stream/flow metaphor. Complements violet-blue without colliding. |
| **Gradient name** | "Undertow" | The pull beneath the surface. Quiet infrastructure. |
| **Gradient values** | #0ea5a8 → #14b8a6 → #38bdf8 | Cyan → teal → sky. |
| **Mark** | `s//` | Parallel streams + regex substitution (literally what slipstream does). Compact at small sizes. |
| **Full wordmark** | `s//slipstream` | Mark + name, no space between. |
| **Domain (primary)** | slipstream.aetherwing.io | Org subdomain. Vanity domain TBD. |
| **Domain (source)** | github.com/aetherwing-io/slipstream | Canonical repo. |
| **Metaphor** | Aerodynamic drafting | slipstream is the lead vehicle punching through file I/O drag. The LLM drafts behind it at speed — 94% less drag. Think peloton or NASCAR drafting, not infrastructure plumbing. |
| **Personality** | Measured, precise, dry | The quiet rig out front that cuts your drag coefficient by 94%. Doesn't talk about speed — just removes the wind. |

**CSS variable convention — SHARED ORG DNA:**
Use the **same structural names** for backgrounds as mish (`--void`, `--deep`, `--surface`, `--elevated`, `--border`, `--muted`, `--secondary`, `--text`, `--bright`, `--white`) with the **same hex values**. Use **different accent names**: `--cyan`, `--teal`, `--sky`, `--ice` instead of mish's `--violet`, `--indigo`, `--blue`, `--lavender`. Name the gradient `--current` (not `--shift`). Name the text variant `--current-text`.

### Tone and voice

- **Developer-first.** No sales pitch. The tool speaks for itself.
- **Measured, not hype.** Benchmarks are specific and honest ("94% fewer tool calls" not "blazing fast").
- **Pragmatic.** Acknowledges trade-offs honestly.
- **Dry wit allowed.** The name itself is playful — slipstream, drafting behind something faster.

**Real voice sample** (from actual README-style writing — match this register):
> "Traditional approach: your agent opens a file, reads it, makes an edit, writes it back, moves to the next file. Eighteen tool calls for what should be one operation. slipstream loads them all into memory, applies edits in a batch, and flushes atomically. One call. Zero partial states."

### Naming rules — LOCKED

- Always lowercase "slipstream" in running text. **Never** "Slipstream" or "SLIPSTREAM."
- At sentence start: **restructure the sentence to avoid leading with the name.** Write "The daemon handles..." or "With slipstream, agents can..." rather than starting a sentence with "slipstream."
- The mark `s//` never has a space before the wordmark: `s//slipstream` not `s// slipstream`.
- In code contexts (CLI, config), `slipstream` appears as-is.

### Approved taglines — LOCKED

| Tagline | Context |
|---------|---------|
| the in-memory editing daemon | PRIMARY — repo subtitle, docs header, og:description |
| batch your edits. skip the round trips. | ACTION — README hero, landing page |
| one call. all files. | PUNCHY — social, badge, quick ref |
| draft behind the daemon. | CORE METAPHOR — blog titles, landing page, talks. The LLM is the vehicle, file I/O is the drag, slipstream punches through the wind. |
| your files are already in memory. | TECHNICAL — architecture docs, deep dives |
| 18 tool calls → 1. | BENCHMARK — readme badge, social proof |

### Required sections (match this structure)

The brand kit HTML must include these sections, numbered:

**01 Logo System**
- Primary mark `s//slipstream` on dark background (DEFAULT)
- Primary mark on light background (ALT)
- Icon `s//` only, at responsive sizes (128, 64, 48, 32, 20, 16px)
- Knockout version on the Undertow gradient

**02 Social Preview (1280×640)**
- GitHub social preview card mockup
- Dark background, wordmark centered, tagline below, feature pills at bottom
- Subtle atmospheric effects (blurred cyan/teal orbs) for depth

**03 Domain System**
- Primary: slipstream.aetherwing.io
- Source: github.com/aetherwing-io/slipstream
- Org: github.com/aetherwing-io

**04 Color Palette**
- Primary gradient "Undertow" with visual swatch
- Individual accent colors: Cyan (#0ea5a8), Teal (#14b8a6), Sky (#38bdf8), Ice (#a5f3fc) with hex + usage
- Background scale: Void (#09090b), Deep (#0f0f13), Surface (#16161d), Elevated (#1e1e28) — note these are shared with mish
- Semantic colors: Success (#34d399), Warn (#f59e0b), Error (#ef4444) — shared with mish
- Each swatch: color fill, name, hex, usage note

**05 Typography**
- Primary: Geist Mono (shared org standard)
- Secondary: Outfit (shared with mish)
- Spec table: context, font, size, weight, letter spacing for each usage (wordmark, section num, section title, body, terminal, labels, table data)

**06 README Elements**
- Badge row (Rust version, MCP native, MIT license, release)
- Terminal hero blocks showing realistic slipstream interactions (one-shot, session, conflict, pre-build check — use the sample tool calls below)

**07 Voice & Messaging**
- 6 approved taglines with usage context
- 3 voice principles as cards: Measured, Honest, Dry

**08 Usage Guidelines**
- Do / Don't cards
- Naming rules (lowercase, sentence-start restructuring, no space in mark)
- Sibling relationship card: mish (μ|sh, Spectral Shift, energetic) vs slipstream (s//, Undertow, measured)
- Note: shared DNA = Geist Mono, dark backgrounds, numbered sections, monospace labels, aetherwing-io org

**09 Favicon & Icon Sizes**
- Row of icon previews at standard sizes with usage labels

**10 Architecture (BONUS — not in mish)**
- Styled CSS architecture diagram (not ASCII)
- Agent boxes → socket connector → daemon container with internal layers
- Use the Undertow accent for the daemon border

### Technical requirements

- Single HTML file, all CSS in `<style>`, no JS
- Google Fonts loaded via `@import` or `<link>`: Geist Mono (300-700), Outfit (200-700)
- Responsive: works at 768px breakpoint (grids collapse to single column)
- Dark theme default (consistent with org aesthetic)
- Hover states on cards (subtle border color shift using cyan with ~0.25 opacity)
- Terminal blocks: macOS-style dots that color on hover (red #ff5f57, yellow #ffbd2e, green #28ca42), blinking cursor
- Container max-width: 1100px, padding: 80px 48px desktop, 40px 24px mobile
- Use CSS custom properties (`:root` vars) for the full palette
- Terminal syntax colors: `--cyan` for prompts, `--bright` for commands, `--sky` for keys, `--ice` for strings/highlights, `--muted` for comments, `--secondary` for output, `--success`/`--warn`/`--error` for status
- Favicon: inline SVG data URI with `s//` in Undertow gradient on dark rounded rect

### What NOT to do

- Don't copy mish's violet-blue gradient. Use Undertow (cyan/teal/sky).
- Don't use flowery marketing language. This is a dev tool README, not a landing page.
- Don't include JavaScript or external image assets.
- Don't make it look like a SaaS product page. Think: technical reference card with taste.
- Don't use serif fonts anywhere.
- Don't forget the shared DNA: Geist Mono, dark backgrounds, numbered sections, monospace labels.
- Don't capitalize "slipstream" in running text. Ever.

### Sample tool calls (use these in terminal mockups)

**One-shot mode:**
```
$ slipstream exec \
    --files handler.rs buffer.rs edit.rs flush.rs types.rs \
    --ops '["str_replace handler.rs old:\"fn foo\" new:\"fn bar\" replace_all"]' \
    --flush
✓ 5 files · 17 edits · 0 conflicts · 3.2ms
```

**Session mode:**
```
$ slipstream session open handler.rs lib.rs --as worker-1
session:worker-1 → 2 files loaded

$ slipstream query "read handler.rs start:100 end:150"
  100│  fn dispatch_op(&self, op: Op) -> Result<()> {
  101│      match op {
  ...

$ slipstream exec --session worker-1 \
    --ops '["str_replace handler.rs old:\"match op\" new:\"match self.validate(op)\""]'
✓ 1 edit queued

$ slipstream session flush worker-1
✓ flushed handler.rs (1 edit, version 2→3)
⚠ worker-2 has pending edits on handler.rs
```

**Conflict scenario:**
```
$ slipstream session flush worker-2
✗ conflict on handler.rs lines [100-105]
  your edits: [100, 105]
  conflicting: [98, 110] by worker-1
  hint: re-read and retry, or --force
```

**Pre-build check:**
```
$ slipstream query "check build"
⚠ 2 sessions have unflushed edits:
  worker-1: handler.rs (1 edit)
  worker-2: lib.rs, types.rs (3 edits)
→ flush or close before building
```
