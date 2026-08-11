# ADR 0012: A derived library graph, and what "no graph query surface" means

- Status: accepted
- Date: 2026-08-06
- Amends: [ADR 0011](./ADR_0011_ZEN_DOCUMENTS.md) mention semantics, and the
  wording of the corresponding non-goal in [PRODUCT.md](./PRODUCT.md) and the
  repository `AGENTS.md`

## Context

The library is only ever seen as a filtered list. Everything it already knows
about relatedness — which saves share a tag, which saves one zen document
mentions together — is invisible. There is no way to see a cluster, no way to
find an orphan (a save with no tag and no mention, which is effectively lost),
and no way to move sideways from a save into its neighbourhood.

A graph view answers those from data the library already stores. It needs no
new field, no new mutation, and no new sync payload.

ADR 0011 forbids something that sounds exactly like it:

> Mentions are one-way references. There is no backlink index, no graph query
> surface, and no automatic insertion of mentions.

That clause was written to protect three things: that a mention costs one
Markdown link and nothing else; that no second structure has to be kept
consistent with the bodies it was derived from; and that ResearchPocket does
not become the note-graph application PRODUCT.md declines to be. It was not
written to decide whether the owner may look at their own library sideways.

Reading the clause literally forbids the view. Reading it for its purpose
forbids *persisting* or *synchronizing* a mention index — which is a different
and much narrower thing.

## Decision

### The prohibition is on stored structure, not on looking

ADR 0011's clause is narrowed to what it was protecting. The revised text
reads verbatim:

> Mentions are one-way references. No backlink index is persisted or
> synchronized, mentions are never inserted automatically, and no mention
> structure is part of the wire format, the projection, or any publication
> artifact. A view may derive relationships from bodies already in
> memory, provided it stores nothing and the derivation is discarded with the
> view. New `research:` reference kinds require an ADR; the namespace is
> append-only.

The PRODUCT.md and `AGENTS.md` non-goal keeps its wording. A derived view is
not "a note graph" in the sense those documents decline: what they decline is
an application whose organizing structure is the graph, where links are
authored as structure and the graph is the thing being maintained. Mentions
remain prose. Nothing in the library is authored *as* an edge.

### What the graph is

The library graph is a pure, synchronous projection over state already loaded:

- **Nodes** are saved items and their tags. Nothing else — not zen documents,
  not source domains.
- **Item-to-tag edges** come from the item's own tags. Tags act as hubs
  deliberately: the item-to-item "shares a tag" clique is O(n²) and a
  300-item tag alone would be roughly 45,000 edges of mush.
- **Item-to-item edges** are co-mentions: for each zen document, every pair of
  distinct items its body mentions, weighted by how many documents the pair
  co-occurs in, capped per document at `MAX_COMENTION_FANOUT` so a link-dump
  document cannot dominate the layout.
- **Orphans** are item nodes with degree zero.

The node set is the list's result set. The same search, tag selection,
favourite and lifecycle filters drive both projections, so switching between
them never changes membership — only how it is drawn.

### What it must not become

- Nothing is written. No store, no index, no cache that outlives the session,
  no field on an item or a document.
- Nothing is synchronized. The graph contributes no mutation, no envelope, and
  no checkpoint content.
- Nothing is published. Edges are not a publication field, and the publisher's
  negative scans are unaffected because there is no artifact to scan.
- No graph-specific mutation is offered from it. Dragging a node moves a pixel,
  not a record, and there is no explicit item-to-item link type; adding one
  would be a new mutation kind and a new sync payload, and would need its own
  ADR. The inspector may invoke the same item actions as the list, such as
  favorite, edit, delete, and restore. Those actions mutate the item through
  the existing application service and do not persist graph structure.
- It is read-only over zen bodies. The graph reads what the reader already
  reads and resolves mentions the same way.

### Cost of reading bodies

The workspace index deliberately carries no body bytes (ADR 0011), so
co-mention derivation is the second call that materializes them. It runs only
while the graph is on screen, and each document's extracted mention list is
memoized against its replica revision, so an unedited document is read once.
This is an in-memory memo of derived data, not an index: it does not survive
the page, and a document that changes is re-read rather than patched.

## Consequences

- The graph is renderer-agnostic by construction. The projection is a plain
  data structure with unit tests and no drawing concern, so the renderer can
  be replaced without touching derivation — which is what makes shipping
  canvas 2D now, rather than a WebGL stack, a reversible decision rather than
  a commitment.
- Mentions gain a second reader, so the `research:item/<uuid>` form is now
  load-bearing in two places. It was already versioned and append-only.
- Deriving on every projection change costs work proportional to items plus
  mentions. Above `GRAPH_NODE_CEILING` the view asks for a narrower filter
  rather than degrading, and the list remains complete either way.
- Orphan detection gives the library its first answer to "what have I saved
  and lost?", which is the strongest single argument for the view existing.
- A future collections or item-link feature will be tempted to persist edges.
  This ADR is the record that doing so is a separate decision with its own
  migration, publication, and sync consequences.

## Verification

- The graph's node set equals the list's result set for the same search, tag
  selection, favourites, and lifecycle filter.
- An item with no tag and no co-mention is reported as an orphan and can be
  isolated with one control.
- Toggling either edge kind changes edges and orphan counts but never the
  item node set.
- A document mentioning more items than `MAX_COMENTION_FANOUT` contributes a
  bounded number of pairs.
- Closing the graph, or reloading the page, leaves no persisted mention
  structure: the projection stores nothing and the memo is per-session.
